from dataclasses import dataclass, field
from typing import Optional, Dict, Any, List

@dataclass
class SessionConfig:
    proxy_url: Optional[str] = None
    stealth: bool = True
    user_agent: Optional[str] = None
    allow_file_access: bool = False
    allow_private_network: bool = False
    timeout_ms: int = 30000

@dataclass
class AgentAction:
    action: str
    url: Optional[str] = None
    selector: Optional[str] = None
    text: Optional[str] = None
    clear: bool = False
    format: Optional[str] = None
    wait_until: str = "load"

@dataclass
class ActionResult:
    action: str
    success: bool
    payload: Optional[Any] = None
    elapsed_ms: float = 0.0
    error: Optional[str] = None

@dataclass
class SemanticNode:
    id: str
    tag: str
    text: str
    selector: str
    role: Optional[str] = None
    is_interactive: bool = False
    attributes: Dict[str, str] = field(default_factory=dict)
    children: List['SemanticNode'] = field(default_factory=list)

