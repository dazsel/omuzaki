# omuzaki-ops: the job manager

- Plain jobs run plan, work and finish on their own threads, in CPU, GPU and network lanes with a shared wait queue of
  256 (#872).
- Each job has a time cap and a money reservation. Shutdown, and a late result from a thread given up after its grace
  period, never change the document; a cancelled job (by request, its time cap or its money cap) may still apply what
  it finished.
- `Executor::with_job_journal` records every start before `start` returns, queued or running.
