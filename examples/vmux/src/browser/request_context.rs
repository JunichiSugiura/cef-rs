//! Per-browser [`cef::RequestContext`] (see [`examples/osr`](../../../../osr/src/webrender.rs)).

use cef::*;

#[derive(Clone)]
pub struct VmuxRequestContextHandler {}

wrap_request_context_handler! {
    pub struct VmuxRequestContextHandlerBuilder {
        handler: VmuxRequestContextHandler,
    }

    impl RequestContextHandler {}
}

impl VmuxRequestContextHandlerBuilder {
    pub fn build(handler: VmuxRequestContextHandler) -> RequestContextHandler {
        Self::new(handler)
    }
}
