"""Escritor ONNX real, sólo stdlib.

Convierte los tensores del safetensors fuente en un modelo ONNX 1.x válido
(protobuf wire format): ModelProto con un GraphProto que declara cada tensor
como initializer y lo publica por un nodo Identity, con inputs/outputs
ValueInfoProto tipados (opset 13, ir_version 8). El modelo resultante es
cargable por onnxruntime; los bytes de cada initializer van verbatim del
safetensors (little-endian en ambos formatos para F32/F16/BF16/I64/I32/I16/
I8/F64).

Una única ruta de emisión (`stream`) alimenta el preflight (hash sin
escritura) y la conversión (escritura durable con checkpoint por tensor):
ambas pasan exactamente por los mismos bytes. El preflight NO lee el fichero:
el layout completo se computa desde la cabecera validada.
"""
import json
import struct

MAX_PROTO_BYTES = 1 << 30

# ONNX TensorProto data_type
ONNX_F32 = 1
ONNX_I8 = 4
ONNX_I16 = 5
ONNX_I32 = 6
ONNX_I64 = 7
ONNX_F16 = 10
ONNX_F64 = 11
ONNX_BF16 = 16

SAFETENSORS_DTYPES = {
    "F64": ONNX_F64,
    "F32": ONNX_F32,
    "F16": ONNX_F16,
    "BF16": ONNX_BF16,
    "I64": ONNX_I64,
    "I32": ONNX_I32,
    "I16": ONNX_I16,
    "I8": ONNX_I8,
}

ELEMENT_BYTES = {"F64": 8, "F32": 4, "F16": 2, "BF16": 2, "I64": 8, "I32": 4, "I16": 2, "I8": 1}

IR_VERSION = 8
OPSET_VERSION = 13
MAX_HEADER_BYTES = 16 << 20

PRODUCER_NAME = "larm-onnx-product"
GRAPH_NAME = "larm_product_graph"


class ONNXError(ValueError):
    pass


# ------------------------------------------------------------ varint/proto

def _varint(value):
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def _tag(field, wire):
    return _varint((field << 3) | wire)


def _bytes_field(field, payload):
    return _tag(field, 2) + _varint(len(payload)) + payload


def _string_field(field, value):
    return _bytes_field(field, value.encode("utf-8"))


def _varint_field(field, value):
    return _tag(field, 0) + _varint(value)


def _packed_varint_field(field, values):
    return _bytes_field(field, b"".join(_varint(v) for v in values))


# ------------------------------------------------------------ safetensors

class Safetensors:
    def __init__(self, header_len, header, data_start, data_size):
        self.header_len = header_len
        self.header = header
        self.data_start = data_start
        self.data_size = data_size

    def tensors(self):
        for name, entry in self.header.items():
            if name == "__metadata__":
                continue
            yield name, entry


def read_safetensors(opener, expected_size):
    with opener() as source:
        prefix = source.read(8)
        if len(prefix) != 8:
            raise ONNXError("safetensors_prefix_invalid")
        header_len = struct.unpack("<Q", prefix)[0]
        if header_len == 0 or header_len > MAX_HEADER_BYTES or 8 + header_len > expected_size:
            raise ONNXError("safetensors_header_invalid")
        header_raw = source.read(header_len)
        if len(header_raw) != header_len:
            raise ONNXError("safetensors_header_truncated")
        try:
            header = json.loads(header_raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ONNXError("safetensors_header_invalid") from error
        if not isinstance(header, dict):
            raise ONNXError("safetensors_header_invalid")
        data_start = 8 + header_len
        data_size = expected_size - data_start
        metadata = header.get("__metadata__")
        if metadata is not None and not isinstance(metadata, dict):
            raise ONNXError("safetensors_metadata_invalid")
        tensors = 0
        for name, entry in header.items():
            if name == "__metadata__":
                continue
            tensors += 1
            if not isinstance(name, str) or not name or not isinstance(entry, dict):
                raise ONNXError("safetensors_tensor_invalid")
            if set(entry) != {"dtype", "shape", "data_offsets"}:
                raise ONNXError("safetensors_tensor_invalid")
            dtype = entry["dtype"]
            if dtype not in SAFETENSORS_DTYPES:
                raise ONNXError("safetensors_dtype_unsupported")
            shape = entry["shape"]
            if (
                not isinstance(shape, list)
                or not 1 <= len(shape) <= 4
                or any(not isinstance(dim, int) or dim < 0 for dim in shape)
            ):
                raise ONNXError("safetensors_shape_invalid")
            elements = 1
            for dim in shape:
                elements *= dim
            offsets = entry["data_offsets"]
            if (
                not isinstance(offsets, list)
                or len(offsets) != 2
                or any(not isinstance(item, int) or item < 0 for item in offsets)
                or offsets[0] > offsets[1]
                or offsets[1] > data_size
                or offsets[1] - offsets[0] != elements * ELEMENT_BYTES[dtype]
            ):
                raise ONNXError("safetensors_offsets_invalid")
        if tensors == 0:
            raise ONNXError("safetensors_tensor_missing")
        return Safetensors(header_len, header, data_start, data_size)


# ------------------------------------------------------------- emissions

def _tensor_entries(safetensors):
    entries = []
    for name, entry in safetensors.tensors():
        entries.append({
            "name": name,
            "shape": list(entry["shape"]),
            "onnx_dtype": SAFETENSORS_DTYPES[entry["dtype"]],
            "length": entry["data_offsets"][1] - entry["data_offsets"][0],
            "source_offset": entry["data_offsets"][0],
        })
    return entries


def _tensor_proto_head(entry):
    """TensorProto: dims(1, packed), data_type(2), name(8), raw_data(9)."""
    return (
        _packed_varint_field(1, entry["shape"])
        + _varint_field(2, entry["onnx_dtype"])
        + _string_field(8, entry["name"])
        + _tag(9, 2)
        + _varint(entry["length"])
    )


def _value_info(name, shape, dtype):
    # ValueInfoProto{1: name, 3: TypeProto{1: Tensor{1: elem_type, 2: shape}}}.
    # Cada dimensión del shape es una entrada Dimension independiente (field 1
    # con dim_value dentro); una sola entrada con los mensajes concatenados
    # haría último-gana y la forma del modelo cargado sería corrupta.
    dims = b"".join(_bytes_field(1, _varint_field(1, dim)) for dim in shape)
    tensor = _varint_field(1, dtype) + _bytes_field(2, dims)
    type_proto = _bytes_field(1, tensor)
    return _string_field(1, name) + _bytes_field(3, type_proto)


def _node_identity(name):
    # NodeProto: input(1, repeated string), output(2, repeated string),
    # name(3), op_type(4). Los strings van directo como payload del campo.
    return (
        _string_field(1, name)
        + _string_field(2, name + "_out")
        + _string_field(3, name + "_identity")
        + _string_field(4, "Identity")
    )


def _opset_import(version):
    return _varint_field(2, version)


def _graph_body(entries):
    """ModelProto.graph(7) sin los raw_data: la emisión los intercala por
    posición. `graph_head` = todo lo anterior a los initializers; cada
    initializer es prefix(len conocida)+raw_data."""
    head = b""
    for entry in entries:
        head += _bytes_field(1, _node_identity(entry["name"]))
    head += _string_field(2, GRAPH_NAME)
    body_len = len(head)
    tails = []
    for entry in entries:
        # El campo output(12) ES el ValueInfoProto completo que devuelve
        # _value_info; no se re-envuelve.
        tail = _value_info(entry["name"] + "_out", entry["shape"], entry["onnx_dtype"])
        tails.append(_bytes_field(12, tail))
        body_len += len(tail)
    return head, tails


def layout(safetensors):
    """Layout exacto del modelo ONNX: cada initializer es su propio campo
    protobuf tag(5)+len+mensaje (prefix con el varint del raw_data incluido),
    y el body_len del graph_field(7) es computable porque todos los tamaños
    vienen de la cabecera validada del safetensors."""
    entries = _tensor_entries(safetensors)
    head, tails = _graph_body(entries)
    prefixes = [_tensor_proto_head(entry) for entry in entries]
    # Cada initializer es el campo 5 del GraphProto: tag(5,2)+varint(len del
    # TensorProto completo)+TensorProto(prefix+raw_data).
    initializer_heads = [
        _tag(5, 2) + _varint(len(prefix) + entry["length"])
        for prefix, entry in zip(prefixes, entries)
    ]
    body_len = (
        len(head)
        + sum(
            len(head_i) + len(prefix) + entry["length"]
            for head_i, prefix, entry in zip(initializer_heads, prefixes, entries)
        )
        + sum(len(tail) for tail in tails)
    )
    # El graph_field(7) envuelve TODO el body (head + initializers + tails):
    # el stream emite su tag+len antes del body para que nada quede fuera.
    pre = (
        _varint_field(1, IR_VERSION)
        + _string_field(2, PRODUCER_NAME)
        + _bytes_field(8, _opset_import(OPSET_VERSION))
    )
    graph_head = _tag(7, 2) + _varint(body_len) + head
    total = (
        len(pre)
        + len(graph_head)
        + sum(
            len(head_i) + len(prefix) + entry["length"]
            for head_i, prefix, entry in zip(initializer_heads, prefixes, entries)
        )
        + sum(len(tail) for tail in tails)
    )
    return {
        "pre": pre,
        "head": graph_head,
        "initializer_heads": initializer_heads,
        "initializer_prefixes": prefixes,
        "tails": tails,
        "entries": entries,
        "total": total,
    }


def stream(safetensors, opener, ctx, sink, checkpoint=None, on_tensor=None):
    """Emite el modelo ONNX completo hacia `sink(bytes)` leyendo los tensores
    del safetensors fuente. Devuelve (bytes_emitidos, tensores_emitidos)."""
    plan = layout(safetensors)
    written = 0
    with opener() as source:
        def emit(chunk):
            nonlocal written
            sink(chunk)
            written += len(chunk)

        emit(plan["pre"])
        emit(plan["head"])
        for index, entry in enumerate(plan["entries"]):
            ctx.check_cancelled()
            emit(plan["initializer_heads"][index])
            emit(plan["initializer_prefixes"][index])
            source.seek(safetensors.data_start + entry["source_offset"])
            remaining = entry["length"]
            while remaining > 0:
                chunk = source.read(min(48 << 10, remaining))
                if not chunk:
                    raise ONNXError("safetensors_data_truncated")
                remaining -= len(chunk)
                emit(chunk)
            emit(plan["tails"][index])
            if checkpoint is not None:
                checkpoint(written)
            if on_tensor is not None:
                on_tensor(entry["name"], written)
        if written != plan["total"]:
            raise ONNXError("onnx_layout_mismatch")
    return written, len(plan["entries"])
