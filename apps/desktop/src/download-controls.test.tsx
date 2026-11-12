import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup } from "@testing-library/react";
import { ApiClient, type RawRequest, type RawResponse, type Transport } from "./api/client";
import { DownloadControls } from "./download-controls";

afterEach(cleanup);
const id = "0192aaaa-0000-7000-8000-000000000101";
const receipt: RawResponse = { status: 202, body: { job_id: id, status_url: `/api/v1/jobs/${id}`, state: "RUNNING" } };

function clientWith(respond: (request: RawRequest, index: number) => Promise<RawResponse>) {
  const calls: RawRequest[] = [];
  const transport: Transport = async (request) => { calls.push(request); return respond(request, calls.length); };
  return { client: new ApiClient(transport), calls };
}

describe("controles durables de descarga", () => {
  it("pausa sólo por clic, bloquea el doble envío y permite reanudar al observar PAUSED", async () => {
    let resolve!: (value: RawResponse) => void;
    const { client, calls } = clientWith(async (_, index) => index === 1 ? new Promise((done) => { resolve = done; }) : receipt);
    const onRefresh = vi.fn();
    const view = render(<DownloadControls client={client} id={id} state="RUNNING" onRefresh={onRefresh} />);
    expect(calls).toHaveLength(0);
    const button = screen.getByRole("button", { name: "Pausar" });
    fireEvent.click(button); fireEvent.click(button);
    expect(calls).toHaveLength(1);
    expect(calls[0].path).toBe(`/api/v1/downloads/${id}/pause`);
    expect(calls[0].body).toEqual({ reason: "operator_request", approval_id: null });
    resolve(receipt);
    await screen.findByText(/Pausa solicitada/);
    expect(onRefresh).toHaveBeenCalledTimes(1);
    expect((button as HTMLButtonElement).disabled).toBe(true);
    view.rerender(<DownloadControls client={client} id={id} state="PAUSED" onRefresh={onRefresh} />);
    fireEvent.click(screen.getByRole("button", { name: "Reanudar" }));
    await screen.findByText(/Reanudación solicitada/);
    expect(calls).toHaveLength(2);
    expect(calls[1].path).toBe(`/api/v1/downloads/${id}/resume`);
    expect(calls[0].headers?.["Idempotency-Key"]).not.toBe(calls[1].headers?.["Idempotency-Key"]);
  });

  it("espera confirmación antes de cancelar y deja conservar los parciales", async () => {
    const { client, calls } = clientWith(async () => receipt);
    render(<DownloadControls client={client} id={id} state="PAUSED" onRefresh={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Cancelar descarga…" }));
    expect(calls).toHaveLength(0);
    expect(screen.getByText(/Se eliminarán los archivos parciales/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Conservar descarga" }));
    expect(screen.queryByRole("button", { name: "Confirmar cancelación" })).toBeNull();
    expect(calls).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Cancelar descarga…" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirmar cancelación" }));
    await screen.findByText(/Cancelación solicitada/);
    expect(calls).toHaveLength(1);
    expect(calls[0].path).toBe(`/api/v1/downloads/${id}/cancel`);
  });

  it("recupera una respuesta perdida con exactamente la misma orden y clave", async () => {
    const { client, calls } = clientWith(async (_, index) => index === 1 ? { status: 0, body: null } : receipt);
    const view = render(<DownloadControls client={client} id={id} state="RUNNING" onRefresh={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Pausar" }));
    await screen.findByRole("button", { name: "Comprobar solicitud" });
    // El polling puede observar otro estado antes de recuperar el recibo.
    view.rerender(<DownloadControls client={client} id={id} state="PAUSED" onRefresh={vi.fn()} />);
    expect((screen.getByRole("button", { name: "Reanudar" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Cancelar descarga…" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Comprobar solicitud" }));
    await screen.findByText(/Pausa solicitada/);
    expect(calls).toHaveLength(2);
    expect(calls[1]).toEqual(calls[0]);
  });

  it("muestra conflicto sin afirmar que la orden fue aceptada", async () => {
    const { client } = clientWith(async () => ({status:409,body:{code:"INVALID_STATE_TRANSITION",message:"state changed",retryable:false,correlation_id:"c",details:{reason:"download_control_state_not_allowed"}}}));
    const onRefresh = vi.fn();
    render(<DownloadControls client={client} id={id} state="RUNNING" onRefresh={onRefresh} />);
    fireEvent.click(screen.getByRole("button", { name: "Pausar" }));
    expect((await screen.findByRole("alert")).textContent).toContain("Actualiza la cola");
    expect(screen.queryByText(/Pausa solicitada/)).toBeNull();
    expect(onRefresh).not.toHaveBeenCalled();
  });

  it("un recibo de otro trabajo deja la solicitud pendiente de comprobación", async () => {
    const { client } = clientWith(async () => ({status:202,body:{job_id:"other",status_url:"/api/v1/jobs/other",state:"RUNNING"}}));
    render(<DownloadControls client={client} id={id} state="RUNNING" onRefresh={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Pausar" }));
    await screen.findByRole("button", { name: "Comprobar solicitud" });
    expect(screen.queryByText(/Pausa solicitada/)).toBeNull();
  });

  it.each(["COMPLETED", "CANCELLED", "CANCELLING"])("no ofrece acciones nuevas en %s", async (state) => {
    const { client, calls } = clientWith(async () => receipt);
    render(<DownloadControls client={client} id={id} state={state} onRefresh={vi.fn()} />);
    await waitFor(() => expect(screen.queryByRole("button")).toBeNull());
    expect(calls).toHaveLength(0);
  });
});
