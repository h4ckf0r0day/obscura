# Process Containment Evidence

- Linux containment: worker sets its own `PR_SET_PDEATHSIG(SIGKILL)` at startup;
  detached reaper (setsid) kills the process group on agent death.
- macOS containment: detached reaper only.
- Scenario 5 verified browser and descendants terminate after agent SIGKILL.
- Scenario 3 verified lease-loss termination.
- No process-group-wide kills affect other workers (scoped by worker PID).