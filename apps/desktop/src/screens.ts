/** Las 14 pantallas del contrato 08 (spec §106), en su orden de menú. */
export interface ScreenDef {
  id: string;
  title: string;
  /** Prompts que construyen su contenido; hasta entonces la pantalla lo dice. */
  owners: string;
}

export const SCREENS: readonly ScreenDef[] = [
  { id: "inicio", title: "Inicio", owners: "P120" },
  { id: "descubrir", title: "Descubrir", owners: "P044, P121" },
  { id: "mis-modelos", title: "Mis modelos", owners: "P042, P122" },
  { id: "runtimes", title: "Runtimes", owners: "P030–P037, P123" },
  { id: "instancias", title: "Instancias", owners: "P033, P124" },
  { id: "hardware", title: "Hardware", owners: "P010–P013, P125" },
  { id: "almacenamiento", title: "Almacenamiento", owners: "P014–P018, P126" },
  { id: "descargas", title: "Descargas", owners: "P051, P052" },
  { id: "benchmarks", title: "Benchmarks", owners: "P110–P112" },
  { id: "actualizaciones", title: "Actualizaciones", owners: "P032, P050" },
  { id: "fuentes", title: "Fuentes", owners: "P043–P049" },
  { id: "adaptadores", title: "Adaptadores", owners: "P020–P028" },
  { id: "notificaciones", title: "Notificaciones", owners: "P050, P113" },
  { id: "configuracion", title: "Configuración", owners: "P003, P141" },
] as const;

/** Pantalla auxiliar, fuera del menú principal. */
export const DIAGNOSTICS: ScreenDef = { id: "diagnostico", title: "Diagnóstico de conexión", owners: "P006" };

export function screenById(id: string): ScreenDef {
  return SCREENS.find((s) => s.id === id) ?? (id === DIAGNOSTICS.id ? DIAGNOSTICS : SCREENS[0]);
}
