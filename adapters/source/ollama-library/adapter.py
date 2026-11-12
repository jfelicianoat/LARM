"""Provider-specific entry point for OLLAMA_LIBRARY."""
from p049_adapter import create_adapter


class Adapter:
    def __init__(self, package, work, manifest, contracts):
        self.delegate = create_adapter("OLLAMA_LIBRARY", package, work, manifest, contracts)

    def call(self, method, params, ctx):
        return self.delegate.call(method, params, ctx)

    def shutdown(self):
        return self.delegate.shutdown()
