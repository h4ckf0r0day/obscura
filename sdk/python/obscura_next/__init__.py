"""
Obscura Next Python SDK
High-performance headless browser automation and AI-agent runtime client.
"""

from .client import ObscuraClient, AsyncObscuraClient
from .models import SessionConfig, AgentAction, ActionResult, SemanticNode

__version__ = "0.1.0"
__all__ = [
    "ObscuraClient",
    "AsyncObscuraClient",
    "SessionConfig",
    "AgentAction",
    "ActionResult",
    "SemanticNode",
]

