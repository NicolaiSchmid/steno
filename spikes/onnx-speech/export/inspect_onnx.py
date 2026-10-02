#!/usr/bin/env python3
"""Print metadata, IR/opset, I/O shapes and the relative-position table size of a Parakeet encoder export."""
import sys
import onnx
from onnx import numpy_helper

for path in sys.argv[1:]:
    m = onnx.load(path, load_external_data=False)
    print(f"== {path}")
    print("ir_version", m.ir_version, "opset", [(o.domain or "ai.onnx", o.version) for o in m.opset_import], "producer", m.producer_name, m.producer_version)
    print("metadata", {p.key: p.value for p in m.metadata_props})
    for io in list(m.graph.input) + list(m.graph.output):
        dims = [d.dim_param or d.dim_value for d in io.type.tensor_type.shape.dim]
        print(" io", io.name, io.type.tensor_type.elem_type, dims)
    # The rel-pos table is a 3-D fp32 initializer (1, 2*max_len-1, d_model) feeding a Slice.
    for init in m.graph.initializer:
        if len(init.dims) == 3 and init.dims[0] == 1 and init.dims[1] > 1000:
            print(" pos table candidate", init.name, list(init.dims), "external" if init.data_location == 1 else "inline")
    n_ext = sum(1 for i in m.graph.initializer if i.data_location == 1)
    from collections import Counter
    print(" nodes", len(m.graph.node), "initializers", len(m.graph.initializer), "external", n_ext)
    print(" op types", Counter(n.op_type for n in m.graph.node).most_common(12))
