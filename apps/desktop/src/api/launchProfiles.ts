import type { ApiClient } from "./client";
import type { Page } from "./runtimes";

export type LaunchProfileKind = "ECO" | "BALANCED" | "PERFORMANCE" | "LONG_CONTEXT" | "CUSTOM";

export interface LaunchProfileCreate {
  name: LaunchProfileKind;
  parameters: Record<string, unknown>;
  backend_id: string | null;
}

export interface LaunchProfile extends LaunchProfileCreate {
  id: string;
  created_at: string;
  updated_at: string;
  row_version: number;
  metadata: Record<string, unknown>;
  profile_hash: string;
}

export async function launchProfiles(client: ApiClient): Promise<LaunchProfile[]> {
  const profiles: LaunchProfile[] = [];
  let cursor: string | null = null;
  do {
    const params = new URLSearchParams({ limit: "200" });
    if (cursor) params.set("cursor", cursor);
    const page = await client.get<Page<LaunchProfile>>(`/api/v1/launch-profiles?${params}`);
    profiles.push(...page.items);
    cursor = page.next_cursor;
  } while (cursor);
  return profiles;
}
