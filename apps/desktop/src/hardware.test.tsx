import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import App from "./App";
import type { RawRequest, RawResponse, Transport } from "./api/client";
import { exactBytes, formatBytes, type HardwareProfile } from "./hardware";
import profilePcia from "../../../tests/fixtures/hardware/profile-pcia.json";

afterEach(() => {
  cleanup();
  window.location.hash = "";
});

const LIVE = { status: 200, body: { status: "LIVE", schema_version: "1" } };
const READY = { status: 200, body: { status: "READY", schema_version: "1" } };
const FIRST = "GET /api/v1/hardware?limit=200";

/** API falsa solo para pruebas: cada ruta responde con la siguiente respuesta de su cola. */
function transport(routes: Record<string, RawResponse[]>): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const t = (async (req: RawRequest) => {
    calls.push(req);
    const key = `${req.method} ${req.path}`;
    if (key === "GET /api/v1/health/live") return LIVE;
    if (key === "GET /api/v1/health/ready") return READY;
    const queue = routes[key];
    if (!queue || queue.length === 0) throw new Error(`ruta no prevista: ${key}`);
    return queue.length > 1 ? queue.shift()! : queue[0];
  }) as Transport & { calls: RawRequest[] };
  t.calls = calls;
  return t;
}

/** Perfil con la forma de la API (valores del sondeo real del PC IA). */
function profile(over: Partial<HardwareProfile["snapshot"]["ram"]> = {}, unknown: HardwareProfile["metadata"]["unknown"] = []): HardwareProfile {
  return {
    id: "0192aaaa-0000-7000-8000-000000000001",
    row_version: 3,
    hardware_fingerprint: "a780bd6b6ca55c9552a900b7c950ae06c8fc34bc9bbefdd1385a42a13ee5d2a7",
    probed_at: "2026-10-11T09:41:01.506Z",
    evidence_refs: ["larm-hardware@0.1.0"],
    metadata: {
      profiler: { name: "larm-hardware", version: "0.1.0" },
      sources: {
        os: "registry:HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
        cpu: "sysinfo@0.39.6:GetSystemInfo",
        "cpu.instruction_sets": "cpuid:is_x86_feature_detected",
        "ram.physical_bytes": "powershell:Get-CimInstance:Win32_PhysicalMemory",
        "ram.usable_bytes": "sysinfo@0.39.6:GlobalMemoryStatusEx",
        pagefile: "sysinfo@0.39.6:GetPerformanceInfo",
      },
      pagefile: { total_bytes: "4294967296" },
      coverage: { probed: ["os", "cpu", "ram"], pending: ["devices", "dependencies"] },
      unknown,
    },
    snapshot: {
      os: { edition: "Professional", build: "26200.9457", architecture: "AMD64" },
      cpu: {
        vendor: "AuthenticAMD",
        model: "AMD RYZEN AI MAX+ 395 w/ Radeon 8060S",
        physical_cores: 16,
        logical_cores: 32,
        instruction_sets: ["avx", "avx2", "avx512f"],
      },
      ram: { physical_bytes: "137438953472", usable_bytes: "68340748288", pool_id: "system-ram", ...over },
      devices: [],
      dependencies: [],
      observed_metrics: { available_ram_bytes: "46024388608", cpu_usage_percent: 4.7 },
    },
  };
}

const page = (items: HardwareProfile[], next: string | null = null): RawResponse => ({
  status: 200,
  body: { items, next_cursor: next, snapshot_at: "2026-10-11T09:42:00.000Z" },
});

function open(t: Transport) {
  window.location.hash = "#/hardware";
  render(<App transport={t} pollMs={60_000} />);
}

describe("unidades", () => {
  it("IEC con un decimal y bytes exactos, sin perder precisión por encima de 2^53", () => {
    expect(formatBytes("0")).toBe("0 bytes");
    expect(formatBytes("1023")).toBe("1023 bytes");
    expect(formatBytes("1024")).toBe("1,0 KiB");
    expect(formatBytes("68340748288")).toBe("63,6 GiB");
    expect(formatBytes("137438953472")).toBe("128,0 GiB");
    expect(formatBytes("9223372036854775807")).toBe("8,0 EiB");
    expect(exactBytes("9223372036854775807")).toBe("9.223.372.036.854.775.807 bytes");
  });
});

describe("pantalla Hardware", () => {
  it("muestra sistema, CPU y memoria con fuente, estado y unidades IEC", async () => {
    open(transport({ [FIRST]: [page([profile()])] }));
    const ram = await screen.findByRole("region", { name: "Memoria" });
    expect(within(ram).getByText("128,0 GiB")).toBeTruthy();
    expect(within(ram).getByText("(137.438.953.472 bytes)")).toBeTruthy();
    expect(within(ram).getByText("63,6 GiB")).toBeTruthy();
    expect(within(ram).getByText(/la diferencia \(64,4 GiB\) está reservada/)).toBeTruthy();
    expect(within(ram).getByText("Fuente: powershell:Get-CimInstance:Win32_PhysicalMemory")).toBeTruthy();
    const cpu = screen.getByRole("region", { name: "Procesador" });
    expect(within(cpu).getByText("AMD RYZEN AI MAX+ 395 w/ Radeon 8060S")).toBeTruthy();
    expect(within(cpu).getAllByText(/Verificado/).length).toBe(4);
    expect(within(within(cpu).getByRole("list", { name: /Extensiones/ })).getAllByRole("listitem")).toHaveLength(3);
    expect(screen.getByTestId("hw-fingerprint").textContent).toBe(profile().hardware_fingerprint);
    // Lo no sondeado se dice: no se presenta como «no hay GPU».
    const pending = screen.getByRole("region", { name: /GPU, NPU/ });
    expect(within(pending).getByText("Todavía no se sondean.")).toBeTruthy();
  });

  it("un dato desconocido muestra el motivo y no inventa un valor", async () => {
    const p = profile({ physical_bytes: null }, [{ field: "ram.physical_bytes", reason: "wmi_timeout", detail: "x" }]);
    open(transport({ [FIRST]: [page([p])] }));
    const ram = await screen.findByRole("region", { name: "Memoria" });
    const cell = ram.querySelector('[data-field="ram.physical_bytes"]')!;
    expect(cell.textContent).toContain("Desconocido — WMI no respondió a tiempo");
    expect(cell.textContent).not.toContain("GiB");
    expect(within(ram).queryByText(/está reservada/)).toBeNull();
  });

  it("sigue el cursor y muestra la configuración más reciente", async () => {
    const old = { ...profile(), id: "0192aaaa-0000-7000-8000-000000000000", hardware_fingerprint: "0".repeat(64) };
    const t = transport({
      [FIRST]: [page([old], "c1")],
      "GET /api/v1/hardware?limit=200&cursor=c1": [page([profile()])],
    });
    open(t);
    await screen.findByText("Revisión 3 · 2 configuraciones registradas");
    expect(screen.getByTestId("hw-fingerprint").textContent).toBe(profile().hardware_fingerprint);
  });

  it("vacío y error con reintento", async () => {
    const t = transport({
      [FIRST]: [
        page([]),
        { status: 403, body: { code: "FORBIDDEN", message: "Falta el scope read", retryable: false, correlation_id: "c", details: { reason: "scope_missing" } } },
        page([profile()]),
      ],
    });
    open(t);
    await screen.findByText("Todavía no hay ningún perfil");
    fireEvent.click(screen.getByRole("button", { name: "Comprobar de nuevo" }));
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText("FORBIDDEN (scope_missing)")).toBeTruthy();
    fireEvent.click(within(alert).getByRole("button", { name: "Reintentar" }));
    await screen.findByRole("region", { name: "Memoria" });
    expect(t.calls.filter((c) => c.path.startsWith("/api/v1/hardware"))).toHaveLength(3);
  });
});


/** Perfil real del PC IA (daemon `hardware-probe`, P011). */
const real = () => structuredClone(profilePcia) as unknown as HardwareProfile;

describe("pantalla Hardware: GPU, NPU y APIs (P011)", () => {
  it("dispositivos reales, APIs verificadas por dispositivo y matriz completa", async () => {
    open(transport({ [FIRST]: [page([real()])] }));
    const devices = await screen.findByRole("region", { name: "GPU y NPU" });
    const gpu = within(devices).getByRole("article", { name: /GPU AMD Radeon/ });
    expect(within(gpu).getByText("32.0.21030.2001")).toBeTruthy();
    expect(within(gpu).getByText(/Sale de la RAM del sistema/)).toBeTruthy();
    const apis = within(within(gpu).getByRole("list", { name: /APIs verificadas/ })).getAllByRole("listitem").map((li) => li.textContent);
    expect(apis).toEqual(["DirectML", "OpenCL", "ROCm (HIP)", "Vulkan", "Windows ML"]);
    const npu = within(devices).getByRole("article", { name: /NPU/ });
    expect(within(npu).getByText("Ninguna verificada")).toBeTruthy();

    const matrix = within(screen.getByRole("region", { name: "APIs de aceleración" })).getByRole("table");
    const rows = within(matrix).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(12);
    const status = (api: string) => rows.find((r) => r.getAttribute("data-api") === api)!.querySelector("td")!.textContent;
    expect(status("vulkan")).toBe("Funciona (verificada)");
    expect(status("cuda")).toBe("No instalada");
    expect(within(screen.getByRole("region", { name: "Componentes de runtime" })).getByText("directml.dll")).toBeTruthy();
    expect(screen.queryByText("Todavía no se sondean.")).toBeNull();
  });

  it("sin sondeo de dispositivos lo dice y no afirma que no haya GPU", async () => {
    const p = real();
    p.snapshot.devices = [];
    p.metadata.unknown = [{ field: "devices", reason: "native_timeout", detail: "x" }];
    open(transport({ [FIRST]: [page([p])] }));
    const devices = await screen.findByRole("region", { name: "GPU y NPU" });
    expect(within(devices).getByRole("note").textContent).toContain("no respondió a tiempo");
    expect(within(devices).queryByText(/No hay ninguna GPU/)).toBeNull();
  });

  it("volver a sondear: POST con Idempotency-Key, sigue el job y recarga", async () => {
    const id = "0192aaaa-0000-7000-8000-0000000000aa";
    const newer = { ...real(), row_version: 2 };
    const t = transport({
      [FIRST]: [page([real()]), page([newer])],
      "POST /api/v1/hardware/refresh": [{ status: 202, body: { job_id: id, status_url: `/api/v1/jobs/${id}`, state: "QUEUED" } }],
      [`GET /api/v1/jobs/${id}`]: [
        { status: 200, body: { job_id: id, state: "RUNNING", error: null } },
        { status: 200, body: { job_id: id, state: "COMPLETED", error: null } },
      ],
    });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Volver a sondear" }));
    await screen.findByText("Sondeo terminado y guardado.", {}, { timeout: 5000 });
    await screen.findByText(/Revisión 2/);
    const post = t.calls.find((c) => c.method === "POST")!;
    expect(post.body).toEqual({ resource_id: null, profile_id: null });
    expect(post.headers?.["idempotency-key"]).toMatch(/^hw-refresh-[0-9a-f-]{36}$/);
  }, 10_000);

  it("volver a sondear sin permiso o con un sondeo fallido avisa", async () => {
    const forbidden = { status: 403, body: { code: "FORBIDDEN", message: "Falta admin", retryable: false, correlation_id: "c", details: { reason: "scope_missing" } } };
    open(transport({ [FIRST]: [page([real()])], "POST /api/v1/hardware/refresh": [forbidden] }));
    fireEvent.click(await screen.findByRole("button", { name: "Volver a sondear" }));
    expect((await screen.findByRole("alert")).textContent).toContain("permisos de administrador");
    cleanup();

    const id = "0192aaaa-0000-7000-8000-0000000000bb";
    open(
      transport({
        [FIRST]: [page([real()])],
        "POST /api/v1/hardware/refresh": [{ status: 202, body: { job_id: id, status_url: `/api/v1/jobs/${id}`, state: "RUNNING" } }],
        [`GET /api/v1/jobs/${id}`]: [{ status: 200, body: { job_id: id, state: "FAILED", error: { code: "INTERNAL", message: "El sondeo de hardware falló" } } }],
      }),
    );
    fireEvent.click(await screen.findByRole("button", { name: "Volver a sondear" }));
    expect((await screen.findByRole("alert", {}, { timeout: 5000 })).textContent).toContain("El sondeo de hardware falló");
  }, 10_000);
});

describe("pantalla Hardware: almacenamiento (P012)", () => {
  it("volúmenes reales por GUID, sin letra, y rutas de red disponibles o no", async () => {
    open(transport({ [FIRST]: [page([real()])] }));
    const section = await screen.findByRole("region", { name: "Almacenamiento" });
    const rows = within(section).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(5);
    const c = rows.find((r) => r.getAttribute("data-volume") === "fb4f4fbc-5441-4427-a73a-0e1ea94ad8d3")!;
    expect(c.textContent).toContain("C:");
    expect(c.textContent).toContain("NVMe");
    expect(c.textContent).toMatch(/libres de 1,8 TiB/);
    expect(rows.filter((r) => r.textContent?.includes("Sin letra"))).toHaveLength(2);
    const nas = rows.find((r) => r.getAttribute("data-volume") === String.raw`\\noexiste-larm\x`)!;
    expect(nas.textContent).toContain("No disponible");
    expect(nas.textContent).toContain("Desconocido");
    expect(rows.find((r) => r.getAttribute("data-volume") === String.raw`\\localhost\temp`)!.textContent).toContain("Disponible");
  });

  it("si no se pudieron enumerar los volúmenes lo dice", async () => {
    const p = real();
    p.metadata.storage = { error: "storage_probe_timeout", detail: "x" };
    open(transport({ [FIRST]: [page([p])] }));
    const section = await screen.findByRole("region", { name: "Almacenamiento" });
    expect(within(section).getByRole("note").textContent).toContain("storage_probe_timeout");
  });
});

describe("pantalla Hardware: huella (P013)", () => {
  it("muestra la versión del cálculo y los cambios respecto a la configuración anterior", async () => {
    const p = real();
    p.metadata.fingerprint = {
      version: 1,
      previous_profile_id: "0192aaaa-0000-7000-8000-000000000010",
      previous_fingerprint: "0".repeat(64),
      changes: [{ field: "os.build", before: "26200.9000", after: "26200.9457" }],
    };
    open(transport({ [FIRST]: [page([p])] }));
    const changes = await screen.findByTestId("hw-changes");
    expect(changes.textContent).toContain("os.build");
    expect(changes.textContent).toContain('"26200.9000" → "26200.9457"');
    expect(screen.getByText("Versión 1 del cálculo")).toBeTruthy();
  });

  it("primera configuración y configuración sin cambios", async () => {
    const p = real();
    p.metadata.fingerprint = { version: 1, previous_profile_id: null, previous_fingerprint: null, changes: [] };
    open(transport({ [FIRST]: [page([p])] }));
    expect((await screen.findByTestId("hw-changes")).textContent).toBe("Primera configuración registrada.");
  });
});
