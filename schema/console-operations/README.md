# Console operations

Every GraphQL operation the console sends, by its frozen name. The Leptos console builds its requests
from `cynic` structs in `crates/hive-console/src/api`, which the compiler checks against
`schema/hive.graphql`; `cynic` names an operation after its root struct.

These documents serve two checks:

- `npm run check:console:operations` holds the operation names here equal to the console's root
  structs. Audit rows, service logs, and the end-to-end checks identify a request by this name.
- `npm run check:schema:contract` compares every type these operations can reach with
  `schema/contract.graphql`.

The selection sets record what the original console selected. The `cynic` structs select a subset.
