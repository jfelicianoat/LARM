import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ModelPromote } from "./model-promote";
import { ApiClient, type RawRequest, type RawResponse, type Transport } from "./api/client";

afterEach(() => cleanup());

function transport(handler: (request: RawRequest) => RawResponse | Promise<RawResponse>): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const value = (async (request: RawRequest) => { calls.push(request); return handler(request); }) as Transport & { calls: RawRequest[] };
  value.calls = calls;
  return value;
}

const variantId = "0192aaaa-0000-7000-8000-0000000000aa";

describe("panel de promoción P120", () => {
  it("asciende un escalón con confirmación y espera la row_version correcta", async () => {
    const t = transport((request) => {
      expect(request.method).toBe("PATCH");
      expect(request.path).toBe(`/api/v1/variants/${variantId}/promotion`);
      expect(request.body).toMatchObject({ promotion: "CANDIDATE", expected_row_version: 4 });
      return { status: 200, body: { variant: {}, row_version: 5 } };
    });
    const completed = vi.fn();
    render(<ModelPromote client={new ApiClient(t)} variantId={variantId} promotion="NONE" rowVersion={4} onCompleted={completed} />);

    fireEvent.click(screen.getByRole("button", { name: "Promocionar a Candidata" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirmar promoción" }));

    expect(await screen.findByText("La variante es ahora: Candidata.")).toBeTruthy();
    expect(completed).toHaveBeenCalledTimes(1);
  });

  it("avisa que la DEFAULT previa retrocede al promocionar a DEFAULT", async () => {
    const t = transport(() => { throw new Error("no debe llamarse en este test"); });
    render(<ModelPromote client={new ApiClient(t)} variantId={variantId} promotion="EVALUATION_APPROVED" rowVersion={9} onCompleted={() => undefined} />);

    fireEvent.click(screen.getByRole("button", { name: "Promocionar a Predeterminada" }));
    expect(screen.getByText(/La variante predeterminada anterior de esta revisión y formato pasará a Aprobada en evaluación/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Confirmar promoción" })).toBeTruthy();
  });

  it("rechazado exige confirmación lateral y muestra el error 409", async () => {
    const t = transport((request) => {
      expect(request.body).toMatchObject({ promotion: "REJECTED", expected_row_version: 7 });
      return { status: 409, body: { code: "StaleRevision", message: "La variante cambió", retryable: false, correlation_id: "c1", details: { reason: "stale_revision" } } };
    });
    render(<ModelPromote client={new ApiClient(t)} variantId={variantId} promotion="CANDIDATE" rowVersion={7} onCompleted={() => undefined} />);

    fireEvent.click(screen.getByRole("button", { name: "Marcar como Rechazada…" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirmar Rechazada" }));

    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.getByText("La variante cambió mientras decidías. Revisa su estado y vuelve a intentarlo.")).toBeTruthy();
  });
});
