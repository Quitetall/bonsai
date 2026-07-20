# Bonsai framework contract

Bonsai is a general-purpose complexity-management framework for infrastructure and production repositories. It is useful wherever a repository contains an intended system shape that must survive implementation churn: codecs, control planes, distributed services, compilers, data platforms, firmware, deployment stacks, regulated ML systems, and meta-repositories.

## Authority layers

1. **Blueprint authority** declares slots, typed ports, flows, invariants, compatibility policy, variants, fusion permission, implementation bindings, and witnesses.
2. **Fact authority** stores normalized claims with provenance in immutable snapshots.
3. **Query interfaces** expose bounded BQL and read-only GraphQL without owning truth.
4. **Conformance** combines blueprint locks with the existing structural tree, dependency DAG, contracts, placement, leanness ratchet, hooks, and CI.
5. **Agent integration** supplies deterministic context and verdicts to a harness such as Katana. Models propose and implement; Bonsai verifies.

## Shape identity

Shape identity includes slot identifiers and invariants, fusion policy, port ownership/direction/schema/version/compatibility, and flow topology/state. It excludes lifecycle, prose, implementation bindings, adapters, and witnesses. Consequently:

- replacing an implementation or adding a predictor variant is implementation-only;
- changing a schema, stage order, invariant, or flow creates a new shape;
- a fused schedule may implement several slots without erasing their authored boundaries;
- documentation can improve without invalidating a lock.

## Evolution rule

Topology-breaking work receives a new blueprint identity and may name the old identity with `supersedes`. An old external representation remains supported through a direct adapter to the current port plus declared witnesses. Adapter-to-adapter chains are rejected. This makes the compatibility burden explicit and keeps any shim one mechanical level below the stable abstraction.

## Adapter direction

The core fact vocabulary is intentionally small: entity subjects, typed objects, predicates, attributes, and source references. Adapters should normalize facts from authoritative producers rather than parse presentation text when a compiler or schema artifact exists. Blueprint and SCIP symbol/file-dependency adapters are implemented. Further adapter families are:

- richer compiler call, type, and generated-code relationships;
- package/build graphs and generated-artifact provenance;
- OpenAPI, Protobuf, GraphQL, database, and event schemas;
- Terraform, Kubernetes, Nix, CI/CD, policy, and ownership metadata;
- tests, benchmarks, deployments, incidents, and runtime observations.

Every adapter must name itself, retain a source locator and digest, remain deterministic for equal input, and distinguish unmeasured data from an empty result.

## Operational contract

The supported repository layout is either one `bonsai.blueprint.toml` or multiple `.bonsai/blueprints/*.toml` files with adjacent `*.lock.json` files for locked lifecycles. `bonsai check` discovers these automatically. Local databases and scan caches belong under `.bonsai/` and should not be committed; blueprints, locks, and witness records should be committed.

SQLite is the default local store because it is portable, inspectable, transaction-safe, and requires no service. Content-addressed snapshots make comparisons and agent replay reproducible. A server deployment may wrap the same interfaces later, but local execution remains the semantic floor.

Agent harnesses should refresh the mutable `main` ref with `bonsai graph --root . snapshot --discover --require-fresh-scip` before asking for current context. Discovery consumes conventional blueprint files and available SCIP indices; repositories remain responsible for regenerating compiler indices after code changes. The freshness gate rejects indices older than tracked source files instead of minting a new snapshot around stale compiler facts. Historical refs and content-addressed snapshot IDs are never refreshed in place.
