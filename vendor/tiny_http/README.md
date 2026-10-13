tiny_http 0.12.0, unchanged but for one line in `src/util/task_pool.rs`
(marked `llmpool:`): a new connection gets a thread of its own unless an idle
thread is free for it. Upstream could leave a connection waiting for another
to close when several arrived together.
