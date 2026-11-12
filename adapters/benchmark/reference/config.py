"""Suite privada de referencia; las suites de inferencia de producto siguen aparte."""
CAPABILITY="vendor.reference.echo"
MAPPING="reference.http.echo.v1"
ESTIMATOR="reference.echo.buffers.v1"
SUITE={"id":"reference.http.echo.benchmark.v1","version":"1.0","capability":CAPABILITY,"warmup":1,"repetitions":5,"metric":"end_to_end_http_ms","clock":"monotonic_ns"}
SETTINGS={"warmup":1,"repetitions":5,"seed":42,"temperature":0,"top_p":1,"batch":1,"benchmark_timeout_seconds":1800,"io_timeout_seconds":2,"generation_settings_applied":False}
UNAVAILABLE={"load_ms":"NOT_MEASURED","ttft_ms":"UNSUPPORTED_NON_STREAMING","input_tokens":"NOT_EXPOSED_BY_ECHO","output_tokens":"NOT_EXPOSED_BY_ECHO","generation_duration_ms":"NOT_APPLICABLE","peak_ram_bytes":"BACKEND_PROCESS_NOT_OBSERVED","peak_vram_bytes":"BACKEND_PROCESS_NOT_OBSERVED"}
