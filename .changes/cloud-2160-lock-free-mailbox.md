# The render thread takes scenes without a lock

- `scene::apply::Mailbox` is a lock-free triple buffer (#2160). The render thread takes a posted scene with two atomic exchanges and a swap: it never locks, waits, allocates or frees, and it never skips a post because a poster holds a lock. Posts from several threads still take turns, off the render thread. A stress test makes 20,000 posts against a thread taking as fast as it can. Every scene taken is whole and in order, and the take side allocates nothing.
