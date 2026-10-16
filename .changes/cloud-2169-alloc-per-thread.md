# omuzaki-ai: `slot_session_alloc` counts allocations per thread

- The counting allocator in the `slot_session_alloc` test kept its counter in a global, so two tests measuring at once counted each other's allocations and the test failed in about half the runs. The counter is now per thread, beside the `MEASURING` flag (#2169).
