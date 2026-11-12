import { useCallback, useEffect, useRef, useState } from "react";
import type { ApiClient } from "./api/client";
import { probe, type StatusSnapshot } from "./api/status";

/** Pantalla activa a partir de `#/<id>`; el navegador gestiona el historial. */
export function useHashRoute(): [string, (id: string) => void] {
  const read = () => window.location.hash.replace(/^#\/?/, "") || "inicio";
  const [route, setRoute] = useState<string>(read);
  useEffect(() => {
    const onChange = () => setRoute(read());
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  const go = useCallback((id: string) => {
    window.location.hash = `#/${id}`;
  }, []);
  return [route, go];
}

/** Estado del daemon, comprobado al montar y cada `intervalMs`. */
export function useDaemonStatus(client: ApiClient, intervalMs = 5000): [StatusSnapshot, () => Promise<void>] {
  const [snapshot, setSnapshot] = useState<StatusSnapshot>({ status: "CONNECTING", error: null, checkedAt: null });
  const alive = useRef(true);
  const refresh = useCallback(async () => {
    const next = await probe(client);
    if (alive.current) setSnapshot(next);
  }, [client]);
  useEffect(() => {
    alive.current = true;
    void refresh();
    const timer = window.setInterval(() => void refresh(), intervalMs);
    return () => {
      alive.current = false;
      window.clearInterval(timer);
    };
  }, [refresh, intervalMs]);
  return [snapshot, refresh];
}
