#!/usr/bin/env python3
"""Bonsai docs-plane compose — the *recursive* generalization of the legacy
`tools/doc_compose.py` (ADR 0134).

It reuses the proven legacy helpers (frontmatter parse, atom validation, keyref
transclusion, the DO-NOT-EDIT banner, frontmatter emission constants) and replaces
`render_parent` with a **recursive fold**: a child may be an ATOM (a path under
`docs/atoms/`, the legacy path — unchanged) OR a COMPOSITE (a manifest `node` id),
composed recursively. The depth-1 pin (`doc_compose.py:194-195`) is gone.

Invariant (the no-regression guarantee, ADR 0134): when every child of every parent
is an atom — which is true of all 59 committed parents — the composite branch is
never taken, so the output is **byte-identical** to the legacy compose. Proven by
`main()`, which re-renders every manifest parent and diffs against disk.

Usage:
  python3 bonsai/docs/compose.py            # byte-identity check vs the 59 parents (+ recursion self-test)
"""
import os
import sys
from pathlib import Path

# Reuse the legacy tooling verbatim (its REPO resolves to the meta root correctly).
_TOOLS = Path(__file__).resolve().parents[2] / "tools"
sys.path.insert(0, str(_TOOLS))
import doc_compose as dc  # noqa: E402
import doc_fm  # noqa: E402


def render_body(p, manifest, vocab, hazards, ledger, seen):
    """Compose a parent's ordered children into its body (keyrefs unresolved).
    Returns (body_str, union_topics, errs). Atom path is identical to the legacy
    per-atom loop; composite children recurse (ADR 0134)."""
    errs, bodies, topics = [], [], set()
    pfile = (dc.REPO / p["file"]).resolve()
    by_node = {q.get("node"): q for q in manifest}
    for a in p.get("atoms", []):
        ap = dc.REPO / a
        if ap.exists():  # ── ATOM child (legacy path — byte-identical) ─────────────
            fm, body = dc.split_frontmatter(dc._read(ap))
            if fm.get("kind") != "atom":
                sys.exit(f"bonsai.compose: {a} referenced as an atom but lacks `kind: atom`")
            errs += dc.validate_atom(a, fm, vocab, hazards)
            is_dep = fm.get("status") == "deprecated"
            in_dep = "/deprecated/" in a
            if is_dep != in_dep:
                errs.append(
                    f"{a}: status:deprecated ⇔ a docs/atoms/<sub>/deprecated/ path must agree "
                    f"(status_deprecated={is_dep}, in_deprecated_dir={in_dep})"
                )
            if is_dep:
                continue
            topics.update(doc_fm.as_list(fm.get("topics")))
            sup_links, serrs = dc._supersedes_link(fm, a, pfile)
            errs += serrs
            bodies.append(body.rstrip() + sup_links)
        else:  # ── COMPOSITE child (ADR 0134 recursion) ───────────────────────────
            child = by_node.get(a)
            if child is None:
                sys.exit(
                    f"bonsai.compose: parent {p['file']} references '{a}' which is neither "
                    f"an existing atom path nor a manifest node"
                )
            if a in seen:
                errs.append(f"{p['file']}: composition cycle at node '{a}'")
                continue
            cbody, ctopics, cerrs = render_body(child, manifest, vocab, hazards, ledger, seen | {a})
            errs += cerrs
            topics.update(ctopics)
            title = child.get("title", a)
            bodies.append((f"## {title}\n\n{cbody}").rstrip() if cbody else f"## {title}")
    return "\n\n".join(bodies), topics, errs


def render_parent(p, vocab, hazards, ledger=None, manifest=None):
    """Recursive drop-in for `doc_compose.render_parent`. Byte-identical to the
    legacy output for all-atom parents; supports composite children."""
    pfile = (dc.REPO / p["file"]).resolve()
    if not (pfile == dc.REPO or dc.REPO in pfile.parents):
        sys.exit(f"bonsai.compose: parent file {p['file']} escapes the repo root")

    body, atom_topics, errs = render_body(
        p, manifest if manifest is not None else [p], vocab, hazards, ledger, {p.get("node")}
    )

    topics = sorted(set(p.get("topics", []) or []) | atom_topics)
    for t in topics:
        if t not in vocab:
            errs.append(f"{p['file']}: parent topic '{t}' not in docs/topics.toml")
    if not topics:
        errs.append(f"{p['file']}: composite parent has no topics (need ≥1 from the vocab)")

    parent_target = (dc.REPO / "docs" / p.get("parent", "MASTER.md")).resolve()
    rel_parent = os.path.relpath(parent_target, pfile.parent)
    fm_lines = ["---", f"parent: {rel_parent}", f"node: {p['node']}"]
    for k in ("status", "owner", "diataxis"):
        fm_lines.append(f"{k}: {p.get(k, dc.FM_DEFAULTS[k])}")
    fm_lines += ["kind: composite", f"topics: [{', '.join(topics)}]", "---"]
    front = "\n".join(fm_lines)

    # First *atom* child drives the banner's atoms dir (byte-identical for all-atom
    # parents; robust when the first child is a composite).
    first_atom = next((a for a in p.get("atoms", []) if (dc.REPO / a).exists()), None)
    atoms_rel = "/".join(Path(first_atom).parts[:-1]) if first_atom else "docs/atoms"
    banner = dc.BANNER.format(atoms=atoms_rel)

    body, kerrs = dc.resolve_keyrefs(body, ledger or {})
    errs += [f"{p['file']}: {e}" for e in kerrs]
    title = p.get("title")
    head = f"\n\n# {title}\n" if title else "\n"
    return pfile, f"{front}\n\n{banner}{head}\n{body}\n", errs


def _recursion_selftest(vocab, hazards, ledger):
    """Prove recursion works: a grandparent whose child is a composite parent whose
    child is a real atom. Composes without error and inlines the nested body."""
    real_atom = next(iter(dc.ATOMS_DIR.rglob("*.md")), None)
    if real_atom is None:
        return "skip (no atoms)"
    atom_rel = str(real_atom.relative_to(dc.REPO))
    child = {"file": "docs/_bonsai_selftest_child.md", "node": "bonsai-selftest-child",
             "title": "Bonsai Selftest Child", "atoms": [atom_rel]}
    grand = {"file": "docs/_bonsai_selftest_grand.md", "node": "bonsai-selftest-grand",
             "title": "Bonsai Selftest Grand", "atoms": ["bonsai-selftest-child"]}
    manifest = [child, grand]
    _, text, errs = render_parent(grand, vocab, hazards, ledger, manifest)
    ok = ("## Bonsai Selftest Child" in text) and not errs
    return f"{'PASS' if ok else 'FAIL'} (3-deep grandparent→composite→atom; errs={errs})"


def main():
    parents = dc.load_manifest()
    vocab = dc.load_vocab()
    hazards = dc._hazard_ids()
    ledger = dc.load_ledger()
    mismatch = 0
    for p in parents:
        pfile, rendered, _errs = render_parent(p, vocab, hazards, ledger, parents)
        if not pfile.exists():
            print(f"  MISSING: {p['file']}")
            mismatch += 1
        elif dc._read(pfile) != rendered:
            print(f"  DRIFT:   {pfile.relative_to(dc.REPO)}")
            mismatch += 1
    print(f"byte-identity: {len(parents) - mismatch}/{len(parents)} parents match the legacy output")
    print(f"recursion self-test: {_recursion_selftest(vocab, hazards, ledger)}")
    return 0 if mismatch == 0 else 2


if __name__ == "__main__":
    sys.exit(main())
