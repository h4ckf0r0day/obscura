# Phase 5.10 Network Policy

validate_url now requires allow_file_access for file://. Redirect targets validated with same policy. HTTP/HTTPS remain subject to SSRF guards. Tests: file:// rejected by default, allowed with opt-in.