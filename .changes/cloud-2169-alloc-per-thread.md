# omuzaki-ai: `slot_session_alloc` counts allocations per thread

- The counting allocator in the `slot_session_alloc` test kept its counter in a global, so two tests measuring at once counted each other's allocations and the test failed in about half the runs. The counter is now per thread, beside the `MEASURING` flag (#2169).
- The loader's wait for an unreported crash no longer holds the session's records lock while it takes the registry's, the opposite of a commit's order, which deadlocked under contention; and no fallback is chosen before a crashed model is reported. From #2126 (`8b566da`, `57eae5b`).
