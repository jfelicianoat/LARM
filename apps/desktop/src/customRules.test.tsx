import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ApiClient, type RawRequest, type RawResponse } from "./api/client";
import { CustomRulesPage } from "./customRules";
import { ToastProvider } from "./components";

const initial = {
  schema_version: "1.0" as const,
  rules: [
    { id: "deny-gguf", predicates: [{ field: "format" as const, operator: "eq" as const, value: "gguf" }], effect: "deny" as const, limit_bytes: null, target_storage_location_id: null },
    { id: "cap-size", predicates: [{ field: "capability" as const, operator: "contains" as const, value: "text.generate" }], effect: "limit" as const, limit_bytes: "300", target_storage_location_id: null },
  ],
};

describe("CustomRulesPage", () => {
  it("previsualiza precedencia y guarda con la revisión recibida por ETag", async () => {
    const calls: RawRequest[] = [];
    let current = initial;
    const transport = async (request: RawRequest): Promise<RawResponse> => {
      calls.push(request);
      if (request.method === "GET") return { status: 200, body: current, headers: { etag: '"0"' } };
      if (request.method === "PATCH") {
        current = request.body as typeof initial;
        return { status: 200, body: current, headers: { etag: '"1"' } };
      }
      return { status: 404, body: null };
    };

    render(<ToastProvider><CustomRulesPage client={new ApiClient(transport)} /></ToastProvider>);
    expect((await screen.findByTestId("preview-disposition")).textContent).toBe("Denegada");
    expect(screen.getByText("300 bytes")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Guardar reglas" }));
    await waitFor(() => expect(calls.some((request) => request.method === "PATCH")).toBe(true));
    expect(calls.find((request) => request.method === "PATCH")?.headers).toEqual({ "If-Match": "0" });
    expect(await screen.findByText(/Revisión guardada:/)).toBeTruthy();
  });
});
