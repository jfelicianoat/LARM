"""Codec HTTP independiente de protocol; sólo echo loopback fijado."""
import hashlib
import http.client
import json
import urllib.parse

from larm_adapter_sdk import DomainError
from larm_adapter_sdk.capabilities import canonical


def failure(code,reason,*,retryable=False):
    return DomainError(code,"Medición de referencia no disponible",reason,retryable=retryable)


def unique(pairs):
    value={}
    for key,item in pairs:
        if key in value: raise ValueError("Clave repetida")
        value[key]=item
    return value


def nonfinite(_value):
    raise ValueError("Número no finito")


class Echo:
    def __init__(self,base):
        parsed=urllib.parse.urlsplit(base)
        if parsed.scheme!="http" or parsed.hostname not in ("127.0.0.1","::1") or not parsed.port or parsed.path or parsed.query or parsed.fragment or parsed.username is not None or parsed.password is not None:
            raise failure("FORBIDDEN","reference_benchmark_loopback_required")
        self.host,self.port=parsed.hostname,parsed.port

    def invoke(self,payload,ctx):
        ctx.check_cancelled()
        raw=canonical(payload)
        if len(raw)>16<<10: raise failure("RESOURCE_UNAVAILABLE","reference_benchmark_request_budget_exceeded")
        connection=http.client.HTTPConnection(self.host,self.port,timeout=2)
        try:
            # HTTPConnection no usa proxies, redirects, auth ni cookies del entorno.
            connection.request("POST","/echo",body=raw,headers={"Content-Type":"application/json","Accept-Encoding":"identity"})
            response=connection.getresponse()
            ctx.check_cancelled()
            if response.status in (401,403): raise failure("AUTH_REQUIRED","reference_benchmark_auth_required")
            if 300<=response.status<400: raise failure("FORBIDDEN","reference_benchmark_redirect_denied")
            if response.status!=200: raise failure("UPSTREAM_UNAVAILABLE","reference_benchmark_http_error",retryable=response.status==429 or response.status>=500)
            if response.getheader("Content-Type","").split(";",1)[0].strip().lower()!="application/json":
                raise failure("PROTOCOL_ERROR","reference_benchmark_reply_type_invalid")
            length=response.getheader("Content-Length")
            if length is not None and (not length.isascii() or not length.isdigit() or len(length)>5 or int(length)>32<<10):
                raise failure("RESOURCE_UNAVAILABLE","reference_benchmark_response_budget_exceeded")
            body=bytearray()
            while True:
                ctx.check_cancelled()
                chunk=response.read(min(1024,(32<<10)+1-len(body)))
                ctx.check_cancelled()
                if not chunk: break
                body.extend(chunk)
                if len(body)>32<<10: raise failure("RESOURCE_UNAVAILABLE","reference_benchmark_response_budget_exceeded")
            if length is not None and len(body)!=int(length):
                raise failure("PROTOCOL_ERROR","reference_benchmark_reply_length_invalid")
            try:
                value=json.loads(body.decode("utf-8"),object_pairs_hook=unique,parse_constant=nonfinite)
            except (ValueError,UnicodeError,RecursionError):
                raise failure("PROTOCOL_ERROR","reference_benchmark_json_invalid") from None
            return value,hashlib.sha256(body).hexdigest()
        except (OSError,http.client.HTTPException):
            ctx.check_cancelled()
            raise failure("UPSTREAM_UNAVAILABLE","reference_benchmark_network_failed",retryable=True) from None
        finally:
            connection.close()
