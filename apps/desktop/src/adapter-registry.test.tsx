import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { AdapterRegistryPanel } from "./adapter-registry";
import { ApiClient, type RawRequest, type RawResponse, type Transport } from "./api/client";

afterEach(() => cleanup());

const configured = {
  enabled: true,
  registry_url: "https://registry.example.test/index.json",
  checkpoint: {
    registry_id: "registry-test", registry_url: "https://registry.example.test/index.json",
    sequence: 7, payload_sha256: "a".repeat(64), issued_at: "2026-10-24T10:00:00Z",
    expires_at: "2026-11-24T10:00:00Z", accepted_at: "2026-10-24T10:05:00Z",
    envelope: { payload: { packages: [{ id: "fixture.http" }] } },
  },
  revocations: [{
    registry_id: "registry-test", target_type: "PACKAGE_SHA256" as const, target: "b".repeat(64),
    revoked_at: "2026-10-24T10:00:00Z", reason_code: "MALICIOUS_PACKAGE", source_sequence: 7,
  }],
};

function transport(): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  let statusReads = 0;
  const value = (async (request: RawRequest): Promise<RawResponse> => {
    calls.push(request);
    if (request.method === "GET" && request.path === "/api/v1/adapters/registry") {
      statusReads++;
      return { status: 200, body: configured };
    }
    if (request.method === "POST" && request.path === "/api/v1/adapters/registry/sync") {
      return { status: 200, body: { sequence: 8, package_count: 3, revocation_count: 2, replay: false } };
    }
    throw new Error(`unexpected ${request.method} ${request.path}; status reads ${statusReads}`);
  }) as Transport & { calls: RawRequest[] };
  value.calls = calls;
  return value;
}

describe("registro remoto de adaptadores", () => {
  it("muestra checkpoint y revocaciones durables sin exponer datos de credenciales", async () => {
    const t = transport();
    render(<AdapterRegistryPanel client={new ApiClient(t)} />);

    const panel = await screen.findByRole("region", { name: "Registro remoto" });
    expect(await screen.findByText("registry-test")).toBeTruthy();
    expect(screen.getByText("https://registry.example.test/index.json")).toBeTruthy();
    expect(screen.getByText("Secuencia").parentElement?.textContent).toContain("7");
    expect(withinRevocations(panel).getByText("Paquete malicioso")).toBeTruthy();
    expect(withinRevocations(panel).getByText("b".repeat(64))).toBeTruthy();
    expect(t.calls.filter((call) => call.method === "GET")).toHaveLength(1);
  });

  it("sincroniza con una clave de idempotencia y vuelve a leer el checkpoint", async () => {
    const t = transport();
    render(<AdapterRegistryPanel client={new ApiClient(t)} />);
    const button = await screen.findByRole("button", { name: "Sincronizar registro" });
    fireEvent.click(button);

    expect(await screen.findByText(/Secuencia 8 aceptada/)).toBeTruthy();
    const post = t.calls.find((call) => call.method === "POST");
    expect(post).toMatchObject({ method: "POST", path: "/api/v1/adapters/registry/sync", body: {} });
    expect(post?.headers?.["Idempotency-Key"]).toMatch(/^adapter-registry-sync-/);
    await waitFor(() => expect(t.calls.filter((call) => call.method === "GET")).toHaveLength(2));
  });
});

function withinRevocations(panel: HTMLElement) {
  const section = panel.querySelector('[aria-label="Revocaciones persistidas"]');
  if (!section) throw new Error("missing revocations section");
  return {
    getByText: (text: string) => {
      const element = Array.from(section.querySelectorAll("*")).find((node) => node.textContent === text);
      if (!element) throw new Error(`missing text ${text}`);
      return element;
    },
  };
}
