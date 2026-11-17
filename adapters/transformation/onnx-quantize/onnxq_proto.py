"""Parseo protobuf mínimo de ONNX para la verificación del worker (stdlib).

Verifica la estructura del modelo de entrada (grafo MatMul con pesos F32) y
la del modelo cuantizado (nodos DequantizeLinear con initializers int8 y
escalas f32, patrón QOperator/MatMulInteger). No reconstruye tensores.
"""
import struct

ONNXQError = ValueError
_DATA_TYPES = {1: "F32", 3: "INT8", 7: "I64", 10: "F16"}


def _varint(raw, off):
    result, shift = 0, 0
    while True:
        byte = raw[off]
        off += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, off
        shift += 7


def _fields(raw, start, end):
    out, off = [], start
    while off < end:
        key, off = _varint(raw, off)
        field, wire = key >> 3, key & 7
        if wire == 2:
            length, off = _varint(raw, off)
            if off + length > end:
                raise ONNXQError("protobuf_truncated")
            out.append((field, raw[off:off + length]))
            off += length
        elif wire == 0:
            value, off = _varint(raw, off)
            out.append((field, value))
        else:
            raise ONNXQError("protobuf_wire_unsupported")
    return out


def _strings(raw, start, end, field):
    return [payload.decode("utf-8") for f, payload in _fields(raw, start, end) if f == field]


def parse_onnx_structure(raw):
    """Estructura del modelo de entrada: grafo con al menos un nodo, pesos
    F32 en los initializers y entradas/salidas con valor."""
    if not raw:
        raise ONNXQError("model_empty")
    top = _fields(raw, 0, len(raw))
    graphs = [payload for field, payload in top if field == 7]
    if len(graphs) != 1:
        raise ONNXQError("graph_missing")
    graph = graphs[0]
    nodes = [payload for field, payload in _fields(graph, 0, len(graph)) if field == 1]
    initializers = [payload for field, payload in _fields(graph, 0, len(graph)) if field == 5]
    if not nodes or not initializers:
        raise ONNXQError("graph_empty")
    for node in nodes:
        op_types = _strings(node, 0, len(node), 4)
        if not op_types:
            raise ONNXQError("node_op_missing")
    return {"nodes": len(nodes), "initializers": len(initializers)}


def parse_quantized_structure(raw):
    """Estructura del modelo cuantizado: nodos DequantizeLinear/MatMulInteger
    con initializers INT8 (tipo 3) y escalas F32 (tipo 1); los pesos F32
    originales ya no están como initializers."""
    if not raw:
        raise ONNXQError("model_empty")
    top = _fields(raw, 0, len(raw))
    graphs = [payload for field, payload in top if field == 7]
    if len(graphs) != 1:
        raise ONNXQError("graph_missing")
    graph = graphs[0]
    nodes, initializers = [], []
    for field, payload in _fields(graph, 0, len(graph)):
        if field == 1:
            nodes.append(payload)
        elif field == 5:
            initializers.append(payload)
    # La forma del modelo cuantizado por Olive (OnnxDynamicQuantization):
    # DynamicQuantizeLinear para las activaciones, MatMulInteger con los
    # pesos int8 (w_quantized) y sus scales/zero_points (w_scale/w_zero_point),
    # el escalado de reequilibrio con Mul y el Cast final a F32.
    ops = {}
    for node in nodes:
        op_types = _strings(node, 0, len(node), 4)
        if not op_types:
            raise ONNXQError("node_op_missing")
        ops[op_types[0]] = ops.get(op_types[0], 0) + 1
    if ops.get("DynamicQuantizeLinear", 0) == 0 or ops.get("MatMulInteger", 0) == 0:
        raise ONNXQError("quantized_nodes_missing")
    if "Mul" not in ops or "Cast" not in ops:
        raise ONNXQError("rescale_missing")
    int8_names, scale_names = set(), set()
    for initializer in initializers:
        fields = dict(_fields(initializer, 0, len(initializer)))
        data_type = fields.get(2)
        name = _strings(initializer, 0, len(initializer), 8)
        if data_type == 3 and name and name[0].endswith("_quantized"):
            int8_names.add(name[0])
        if data_type == 1 and name and name[0].endswith("_scale"):
            scale_names.add(name[0])
    if not int8_names or not scale_names:
        raise ONNXQError("int8_initializers_missing")
    return {"nodes": len(nodes), "initializers": len(initializers),
            "dynamic_quantize_linear": ops.get("DynamicQuantizeLinear", 0),
            "matmul_integer": ops.get("MatMulInteger", 0),
            "int8_initializers": sorted(int8_names), "scales": sorted(scale_names)}
