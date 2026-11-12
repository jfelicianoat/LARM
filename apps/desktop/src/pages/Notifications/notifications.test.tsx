import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ApiClient, type RawRequest, type RawResponse } from "../../api/client";
import type { NotificationItem } from "../../api/notifications";
import { dedupeNotifications, NotificationsPage, safeNotificationDestination } from "./index";

afterEach(cleanup);

const sourceId = "0192f000-0000-7000-8000-000000000301";

function notification(overrides: Partial<NotificationItem> = {}): NotificationItem {
  return {
    id: "0192f000-0000-7000-8000-000000000321", created_at: "2026-10-25T10:00:00.000Z",
    updated_at: "2026-10-25T10:00:00.000Z", row_version: 1, metadata: {}, dedupe_key: "notice-1",
    severity: "warning", message_code: "MODEL_UPDATE", resource_id: sourceId,
    action: { route: `/sources/${sourceId}`, resource_id: sourceId }, read_at: null, ...overrides,
  };
}

describe("notifications center and source watches", () => {
  it("deduplicates by contract key and only opens validated local detail routes", () => {
    expect(dedupeNotifications([notification(), notification({ id: "other" })])).toHaveLength(1);
    expect(safeNotificationDestination(notification())).toEqual({ screen: "fuentes", resourceId: sourceId });
    expect(safeNotificationDestination(notification({ action: { route: "https://example.com", resource_id: sourceId } }))).toBeNull();
    expect(safeNotificationDestination(notification({ action: { route: `/models/${sourceId}`, resource_id: "0192f000-0000-7000-8000-000000000302" } }))).toBeNull();
  });

  it("creates a watcher with exact size and compatibility filters, never an automatic download", async () => {
    const calls: RawRequest[] = [];
    const transport = async (request: RawRequest): Promise<RawResponse> => {
      calls.push(request);
      if (request.method === "GET" && request.path.startsWith("/api/v1/notifications")) return { status: 200, body: { items: [], next_cursor: null, snapshot_at: "now" } };
      if (request.method === "GET" && request.path.startsWith("/api/v1/source-watches")) return { status: 200, body: { items: [], next_cursor: null, snapshot_at: "now" } };
      if (request.method === "GET" && request.path.startsWith("/api/v1/sources")) return { status: 200, body: { items: [{ id: sourceId, name: "Fuente fixture", enabled: true }], next_cursor: null, snapshot_at: "now" } };
      if (request.method === "POST" && request.path === "/api/v1/source-watches") return { status: 201, body: { id: "watch-1" } };
      return { status: 404, body: null };
    };
    render(<NotificationsPage client={new ApiClient(transport)} onNavigate={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: "Añadir seguimiento" }));
    fireEvent.change(screen.getByLabelText("Fuente"), { target: { value: sourceId } });
    fireEvent.change(screen.getByLabelText("Capacidades (separadas por coma)"), { target: { value: "chat, vision" } });
    fireEvent.change(screen.getByLabelText("Compatibilidad"), { target: { value: "COMPATIBLE" } });
    fireEvent.change(screen.getByLabelText("Mínimo"), { target: { value: "1024" } });
    fireEvent.change(screen.getByLabelText("Máximo"), { target: { value: "2048" } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar seguimiento" }));
    await waitFor(() => expect(calls.some((call) => call.method === "POST" && call.path === "/api/v1/source-watches")).toBe(true));
    const create = calls.find((call) => call.method === "POST" && call.path === "/api/v1/source-watches");
    expect(create?.headers?.["Idempotency-Key"]).toBeTruthy();
    expect(create?.body).toMatchObject({ source_id: sourceId, interval_seconds: 21_600, policy: {}, enabled: true,
      query: { capabilities: ["chat", "vision"], compatibility: "COMPATIBLE", size_bytes: { minimum: "1024", maximum: "2048" } } });
  });
});
