"""LARM Adapter Protocol 1.0; SDK Python con contratos explícitos y sin Core."""
from .contracts import Contracts, ContractViolation
from .server import CallContext, DomainError, ProtocolServer, RpcError
from .capabilities import CapabilityRegistry

__all__ = ["Contracts", "ContractViolation", "CallContext", "DomainError", "ProtocolServer", "RpcError", "CapabilityRegistry"]
