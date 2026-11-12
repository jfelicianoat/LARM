"""Acceso explícito de workers: roots, componentes y origen; no concede permisos."""
import stat
import urllib.parse
import urllib.request
from pathlib import Path
from .server import DomainError

def fail(code, reason, message="No se pudo transferir el archivo de referencia", *, retryable=False):
    return DomainError(code, message, reason, retryable=retryable)


def relative(value):
    if not isinstance(value, str) or not value or value.startswith("/") or "\\" in value:
        raise fail("VALIDATION_FAILED", "relative_path_invalid")
    parts = value.split("/")
    devices = {"con", "prn", "aux", "nul", "conin$", "conout$"} | {f"{name}{index}" for name in ("com", "lpt") for index in range(1, 10)}
    for part in parts:
        if not part or part in (".", "..") or part.endswith((".", " ")) or any(c in part for c in '<>:"|?*') or any(ord(c) < 32 for c in part) or part.split(".")[0].lower() in devices:
            raise fail("VALIDATION_FAILED", "relative_path_invalid")
    return value


def path_allowed(value, roots, *, writing=False):
    path = Path(value)
    if not path.is_absolute():
        raise fail("FORBIDDEN", "reference_path_not_absolute")
    for candidate in [path, *path.parents]:
        try:
            info = candidate.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
            raise fail("FORBIDDEN", "reference_reparse_denied")
    real = path.resolve(strict=not writing)
    for root in roots:
        root = Path(root)
        if not root.is_absolute():
            raise fail("FORBIDDEN", "reference_root_invalid")
        resolved = root.resolve(strict=True)
        if real.is_relative_to(resolved):
            # La raíz misma tampoco puede ser un reparse point.
            for candidate in [root, *root.parents]:
                info = candidate.lstat()
                if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
                    raise fail("FORBIDDEN", "reference_reparse_denied")
            return real
    raise fail("FORBIDDEN", "reference_path_outside_roots")


def origin(value):
    try:
        parts = urllib.parse.urlsplit(value)
        port = parts.port
    except ValueError:
        raise fail("FORBIDDEN", "reference_origin_invalid") from None
    if parts.scheme not in ("http", "https") or not parts.hostname or parts.username is not None or parts.password is not None or parts.fragment:
        raise fail("FORBIDDEN", "reference_origin_invalid")
    sensitive = ("token", "password", "authorization", "secret", "signature", "credential", "api_key")
    if any(any(word in key.lower() for word in sensitive) for key, _ in urllib.parse.parse_qsl(parts.query)):
        raise fail("FORBIDDEN", "reference_credentials_in_url")
    host = f"[{parts.hostname.lower()}]" if ":" in parts.hostname else parts.hostname.lower()
    return f"{parts.scheme}://{host}:{port or (443 if parts.scheme == 'https' else 80)}"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None


