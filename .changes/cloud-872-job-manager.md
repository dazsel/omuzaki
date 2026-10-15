# omuzaki-ops: the job manager

- Plain jobs run plan, work and finish on their own threads, in CPU, GPU and network lanes with a shared wait queue of
  256 (#872).
- Each job has a time cap and a money reservation; cancel, deadlines and shutdown end it without changing the document.
- `Executor::with_job_journal` records every start before `start` returns, queued or running.
