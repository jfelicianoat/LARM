import type { ApiClient } from "./client";

export interface NotificationItem {
  id: string;
  created_at: string;
  updated_at: string;
  row_version: number;
  metadata: Record<string, unknown>;
  dedupe_key: string;
  severity: "info" | "warning" | "error";
  message_code: string;
  resource_id: string | null;
  action: { route?: unknown; resource_id?: unknown } | null;
  read_at: string | null;
}

export interface Page<T> {
  items: T[];
  next_cursor: string | null;
  snapshot_at: string;
}

export interface SourceWatch {
  id: string;
  created_at: string;
  updated_at: string;
  row_version: number;
  metadata: Record<string, unknown>;
  source_id: string;
  query: Record<string, unknown>;
  cursor: string | null;
  etag: string | null;
  interval_seconds: number;
  policy: Record<string, unknown>;
  next_run_at: string | null;
  enabled: boolean;
}

export interface SourceWatchInput {
  source_id: string;
  query: Record<string, unknown>;
  interval_seconds: number;
  policy: Record<string, unknown>;
  enabled: boolean;
}

export async function listNotifications(client: ApiClient): Promise<NotificationItem[]> {
  const items: NotificationItem[] = [];
  const seen = new Set<string>();
  let cursor: string | null = null;
  for (let page = 0; page < 100; page += 1) {
    const params = new URLSearchParams({ limit: "200" });
    if (cursor) params.set("cursor", cursor);
    const result = await client.get<Page<NotificationItem>>(`/api/v1/notifications?${params}`);
    items.push(...result.items);
    cursor = result.next_cursor;
    if (!cursor) return items;
    if (seen.has(cursor)) throw new Error("repeated notification cursor");
    seen.add(cursor);
  }
  return items;
}

export async function setNotificationRead(client: ApiClient, item: NotificationItem, read: boolean): Promise<NotificationItem> {
  return client.request<NotificationItem>({
    method: "PATCH",
    path: `/api/v1/notifications/${encodeURIComponent(item.id)}`,
    headers: { "If-Match": String(item.row_version) },
    body: { read },
  });
}

export async function listSourceWatches(client: ApiClient): Promise<SourceWatch[]> {
  const items: SourceWatch[] = [];
  const seen = new Set<string>();
  let cursor: string | null = null;
  for (let page = 0; page < 100; page += 1) {
    const params = new URLSearchParams({ limit: "200" });
    if (cursor) params.set("cursor", cursor);
    const result = await client.get<Page<SourceWatch>>(`/api/v1/source-watches?${params}`);
    items.push(...result.items);
    cursor = result.next_cursor;
    if (!cursor) return items;
    if (seen.has(cursor)) throw new Error("repeated watcher cursor");
    seen.add(cursor);
  }
  return items;
}

export async function createSourceWatch(client: ApiClient, input: SourceWatchInput, key: string): Promise<SourceWatch> {
  return client.request<SourceWatch>({ method: "POST", path: "/api/v1/source-watches", headers: { "Idempotency-Key": key }, body: input });
}

export async function updateSourceWatch(client: ApiClient, watch: SourceWatch, input: SourceWatchInput): Promise<SourceWatch> {
  return client.request<SourceWatch>({
    method: "PATCH", path: `/api/v1/source-watches/${encodeURIComponent(watch.id)}`,
    headers: { "If-Match": String(watch.row_version) }, body: input,
  });
}

export async function deleteSourceWatch(client: ApiClient, watch: SourceWatch): Promise<void> {
  await client.request({ method: "DELETE", path: `/api/v1/source-watches/${encodeURIComponent(watch.id)}`, headers: { "If-Match": String(watch.row_version) } });
}
