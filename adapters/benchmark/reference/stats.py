"""Fórmulas del contrato 05; no fabrica muestras ausentes o duraciones cero."""
import math

from larm_adapter_sdk.capabilities import digest


def summarize(samples):
    measured=[(int(item["end_monotonic_ns"])-int(item["start_monotonic_ns"]))/1_000_000 for item in samples if not item["warmup"] and item["success"]]
    if len(measured)!=5 or any(not math.isfinite(v) or v<=0 for v in measured):
        return None
    mean=math.fsum(measured)/5
    deviation=math.sqrt(math.fsum((v-mean)**2 for v in measured)/4)
    spread=2.776*deviation/math.sqrt(5)
    ordered=sorted(measured)
    return {"count":5,"mean":mean,"median":ordered[2],"p95":ordered[math.ceil(.95*5)-1],"sample_stddev":deviation,"ci95_mean":{"lower":max(0.,mean-spread),"upper":mean+spread},"unit":"ms"}


def fingerprint_hash(components):
    # Evidence refs/timestamps son trazabilidad y no forman parte de identidad.
    return digest({name:{key:item[key] for key in ("value","status")} for name,item in components.items()})
