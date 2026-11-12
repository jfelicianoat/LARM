import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ApiClient, type RawRequest, type RawResponse } from "./api/client";
import { BandwidthSettings, LowDiskThresholdSettings, percentToBasisPoints } from "./settings";
import { ToastProvider } from "./components";

afterEach(cleanup);

describe("BandwidthSettings", () => {
  it("guarda el límite agregado positivo usando If-Match y conserva el resto de Settings", async () => {
    const calls: RawRequest[] = [];
    let current = { storage: { download_bandwidth_limit_bytes_per_second: null as string | null, minimum_free_bytes: "1024" }, api: { port: 4567 } };
    const transport = async (request: RawRequest): Promise<RawResponse> => {
      calls.push(request);
      if (request.method === "GET") return { status: 200, body: current, headers: { etag: '"4"' } };
      if (request.method === "PATCH") {
        current = request.body as typeof current;
        return { status: 200, body: current, headers: { etag: '"5"' } };
      }
      return { status: 404, body: null };
    };

    render(<ToastProvider><BandwidthSettings client={new ApiClient(transport)} /></ToastProvider>);
    const unlimited = await screen.findByRole("checkbox", { name: "Sin límite de velocidad" });
    fireEvent.click(unlimited);
    fireEvent.change(screen.getByLabelText("Límite agregado (bytes por segundo)"), { target: { value: "2048" } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar límite" }));

    await waitFor(() => expect(calls.some((request) => request.method === "PATCH")).toBe(true));
    const patch = calls.find((request) => request.method === "PATCH");
    expect(patch?.path).toBe("/api/v1/settings");
    expect(patch?.headers).toEqual({ "If-Match": "4" });
    expect(patch?.body).toMatchObject({ storage: { download_bandwidth_limit_bytes_per_second: "2048", minimum_free_bytes: "1024" }, api: { port: 4567 } });
    expect(await screen.findByText(/Revisión guardada:/)).toBeTruthy();
  });

  it("impide enviar cero", async () => {
    const transport = async (): Promise<RawResponse> => ({ status: 200, body: { storage: { download_bandwidth_limit_bytes_per_second: null } }, headers: { etag: '"1"' } });
    render(<ToastProvider><BandwidthSettings client={new ApiClient(transport)} /></ToastProvider>);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Sin límite de velocidad" }));
    fireEvent.change(screen.getByLabelText("Límite agregado (bytes por segundo)"), { target: { value: "0" } });
    expect(screen.getByRole("button", { name: "Guardar límite" })).toHaveProperty("disabled", true);
  });
});

describe("LowDiskThresholdSettings", () => {
  it("convierte porcentajes exactamente a basis points", () => {
    expect(percentToBasisPoints("15,25")).toBe(1525);
    expect(percentToBasisPoints("0")).toBe(0);
    expect(percentToBasisPoints("100.01")).toBeNull();
  });

  it("guarda umbrales BYTES exactos bajo la revisión actual", async () => {
    const calls: RawRequest[] = [];
    let current = {
      storage: {
        download_bandwidth_limit_bytes_per_second: null,
        low_disk_thresholds: { mode: "BYTES" as const, warning_bytes: "1500", critical_bytes: "500" },
      },
    };
    const transport = async (request: RawRequest): Promise<RawResponse> => {
      calls.push(request);
      if (request.method === "GET") return { status: 200, body: current, headers: { etag: '"8"' } };
      if (request.method === "PATCH") {
        current = request.body as typeof current;
        return { status: 200, body: current, headers: { etag: '"9"' } };
      }
      return { status: 404, body: null };
    };
    render(<ToastProvider><LowDiskThresholdSettings client={new ApiClient(transport)} /></ToastProvider>);
    fireEvent.change(await screen.findByLabelText("Aviso (warning)"), { target: { value: "1600" } });
    fireEvent.change(screen.getByLabelText("Crítico (critical)"), { target: { value: "600" } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar niveles" }));
    await waitFor(() => expect(calls.some((request) => request.method === "PATCH")).toBe(true));
    const patch = calls.find((request) => request.method === "PATCH");
    expect(patch?.headers).toEqual({ "If-Match": "8" });
    expect(patch?.body).toMatchObject({ storage: { low_disk_thresholds: { mode: "BYTES", warning_bytes: "1600", critical_bytes: "600" } } });
  });
});
