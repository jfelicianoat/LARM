import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ApiClient, type RawRequest, type RawResponse, type Transport } from "./api/client";
import { DeclarativeRuntimeWizard, RuntimeWizard } from "./runtime-wizard";

afterEach(cleanup);

const CREDENTIAL = "0192aaaa-0000-7000-8000-000000000002";

function transport(handler: (request: RawRequest) => RawResponse | Promise<RawResponse>) {
  const calls: RawRequest[] = [];
  const api = (async (request: RawRequest) => { calls.push(request); return handler(request); }) as Transport & { calls: RawRequest[] };
  api.calls = calls;
  return api;
}

function renderWizard(api: Transport, onSaved = () => {}) {
  render(<RuntimeWizard client={new ApiClient(api)} onSaved={onSaved} onCancel={() => {}} />);
}

function fillForm() {
  fireEvent.change(screen.getByLabelText("Nombre"), { target: { value: "  Equipo remoto  " } });
  fireEvent.change(screen.getByLabelText("URL base HTTP o HTTPS"), { target: { value: "https://host.example/v1" } });
  fireEvent.change(screen.getByLabelText("Tipo de host"), { target: { value: "vendor.openai-compatible" } });
  fireEvent.change(screen.getByLabelText("Referencia de credencial (opcional)"), { target: { value: CREDENTIAL } });
  fireEvent.change(screen.getByLabelText("Modo de registro"), { target: { value: "ADOPTED" } });
}

describe("asistente de host externo", () => {
  it("explica el permiso, conserva UNKNOWN y envía sólo la referencia de credencial", async () => {
    let saved = false;
    const api = transport(() => ({ status: 201, body: { id: "0192aaaa-0000-7000-8000-000000000001" } }));
    renderWizard(api, () => { saved = true; });
    expect(screen.getByRole("note").textContent).toContain("runtimes:write");
    expect(screen.getByRole("note").textContent).toContain("conectividad desconocida");
    fillForm();
    fireEvent.click(screen.getByRole("button", { name: "Guardar host externo" }));
    await waitFor(() => expect(saved).toBe(true));
    expect(api.calls).toHaveLength(1);
    expect(api.calls[0]).toEqual({
      method: "POST",
      path: "/api/v1/external-hosts",
      body: {
        name: "Equipo remoto",
        base_url: "https://host.example/v1",
        host_type: "vendor.openai-compatible",
        credential_reference_id: CREDENTIAL,
        registration_mode: "ADOPTED",
      },
    });
  });

  it("rechaza URL con query y valores secretos antes de enviar la solicitud", async () => {
    const api = transport(() => ({ status: 201, body: {} }));
    renderWizard(api);
    fillForm();
    fireEvent.change(screen.getByLabelText("URL base HTTP o HTTPS"), { target: { value: "https://host.example/v1?token=secret" } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar host externo" }));
    expect((await screen.findByRole("alert")).textContent).toContain("no puede incluir usuario, contraseña, consulta ni fragmento");
    expect(api.calls).toHaveLength(0);

    fireEvent.change(screen.getByLabelText("URL base HTTP o HTTPS"), { target: { value: "https://host.example/v1" } });
    fireEvent.change(screen.getByLabelText("Referencia de credencial (opcional)"), { target: { value: "raw-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar host externo" }));
    expect((await screen.findByRole("alert")).textContent).toContain("nunca pegues aquí el secreto");
    expect(api.calls).toHaveLength(0);
  });

  it("un 403 conserva el borrador y explica el permiso requerido", async () => {
    const api = transport(() => ({ status: 403, body: { code: "FORBIDDEN", message: "Denied", retryable: false, correlation_id: "c", details: { reason: "scope_missing" } } }));
    renderWizard(api);
    fillForm();
    fireEvent.click(screen.getByRole("button", { name: "Guardar host externo" }));
    expect((await screen.findByRole("alert")).textContent).toContain("no tiene permiso");
    expect((screen.getByLabelText("Nombre") as HTMLInputElement).value).toBe("  Equipo remoto  ");
    expect(api.calls).toHaveLength(1);
  });
});

const DECLARATIVE_DRAFT = {
  schema_version: "1.0",
  manifest: {
    id: "local.example.runtime", version: "1.0.0", family: "runtime", protocol_version: "1.0", publisher: "Local operator",
    executable: { relative_path: "bin/adapter.exe", args: [] }, platforms: ["windows"], architectures: ["x86_64"], methods: ["runtime.start"],
    permissions: { read_roots: ["C:/models"], write_roots: ["C:/models/cache"], network_origins: ["http://127.0.0.1:11434"], process_allowlist: ["engine.exe"], secret_refs: ["larm/runtime/local"] },
    capabilities: ["chat.completions"], package_sha256: "a".repeat(64), signature: null, installation_strategies: ["CUSTOM"],
  },
  version_probe: { executable: "bin/engine.exe", args: ["--version"], parse_mode: "EXACT_TEXT", json_field: null },
  start: { executable: "bin/engine.exe", args: ["--host", "127.0.0.1"], cwd: "bin" },
  stop: { mode: "SIGNAL", endpoint: null },
  health: { url: "http://127.0.0.1:11434/health", expected_status: 200, json_pointer: null, expected_value: null },
  protocol_profile_id: "openai.http", formats: [], capabilities: [], environment: { allowlist: [], variables: [] },
};

describe("asistente de runtime declarativo", () => {
  it("valida y muestra estrategia, comandos y permisos antes de guardar", async () => {
    const api = transport(() => ({ status: 201, body: { id: "0192aaaa-0000-7000-8000-000000000001", adapter_id: "local.example.runtime", version: "1.0.0", family: "runtime" } }));
    render(<DeclarativeRuntimeWizard client={new ApiClient(api)} onCreated={() => {}} onCancel={() => {}} />);
    fireEvent.change(screen.getByLabelText("Documento DeclarativeAdapter (JSON)"), { target: { value: JSON.stringify(DECLARATIVE_DRAFT, null, 2) } });
    fireEvent.click(screen.getByRole("button", { name: "Validar borrador" }));
    expect((await screen.findByRole("status")).textContent).toContain("volverá a validar el contrato completo");
    const preview = screen.getByRole("region", { name: "Vista previa del adaptador" });
    expect(within(preview).getByText("http://127.0.0.1:11434")).toBeTruthy();
    expect(within(preview).getByText(/--host.*127.0.0.1/)).toBeTruthy();
    expect(within(preview).getByText("CUSTOM")).toBeTruthy();
    expect(api.calls).toHaveLength(0);
  });

  it("rechaza JSON inválido, guarda con idempotencia y consulta la prueba", async () => {
    const adapterId = "0192aaaa-0000-7000-8000-000000000001", jobId = "0192aaaa-0000-7000-8000-000000000003";
    const api = transport((request) => {
      if (request.method === "GET") return { status: 200, body: { job_id: jobId, state: "COMPLETED", resource_type: "adapter_installation", resource_id: adapterId, progress: {}, result: null, error: null, evidence_refs: [] } };
      if (request.path.endsWith("/probe")) return { status: 202, body: { job_id: jobId, status_url: `/api/v1/jobs/${jobId}`, state: "QUEUED" } };
      return { status: 201, body: { id: adapterId, adapter_id: "local.example.runtime", version: "1.0.0", family: "runtime" } };
    });
    render(<DeclarativeRuntimeWizard client={new ApiClient(api)} onCreated={() => {}} onCancel={() => {}} />);
    fireEvent.change(screen.getByLabelText("Documento DeclarativeAdapter (JSON)"), { target: { value: "{" } });
    fireEvent.click(screen.getByRole("button", { name: "Validar borrador" }));
    expect((await screen.findByRole("alert")).textContent).toContain("no es JSON válido");
    expect(api.calls).toHaveLength(0);

    fireEvent.change(screen.getByLabelText("Documento DeclarativeAdapter (JSON)"), { target: { value: JSON.stringify(DECLARATIVE_DRAFT) } });
    fireEvent.click(screen.getByRole("button", { name: "Validar borrador" }));
    await screen.findByRole("status");
    fireEvent.click(screen.getByRole("button", { name: "Guardar adaptador" }));
    expect(await screen.findByText(/Adaptador registrado/)).toBeTruthy();
    expect(api.calls[0].headers?.["Idempotency-Key"]).toBeTruthy();
    expect(api.calls[0].body).toEqual(DECLARATIVE_DRAFT);

    fireEvent.click(screen.getByRole("button", { name: "Solicitar prueba de conformidad" }));
    expect(await screen.findByText(/Job/)).toBeTruthy();
    expect(api.calls[1].path).toBe(`/api/v1/adapters/${adapterId}/probe`);
    expect(api.calls[1].headers?.["Idempotency-Key"]).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Consultar prueba" }));
    expect(await screen.findByText(/La prueba terminó: COMPLETED/)).toBeTruthy();
    expect(api.calls[2].path).toBe(`/api/v1/jobs/${jobId}`);
  });
});
