# Capacity Reconciliation Tests

## Tests Added
- test_capacity_normal_assignment: PASS (real PG)
- test_capacity_cancel_decrements: PASS (real PG)
- test_capacity_reconciliation: PASS (real PG)

## Evidence
- cargo test -p obscura-control-plane capacity: 3/3 PASS
