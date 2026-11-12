/**
 * Cliente de la API administrativa de LARM (P006).
 *
 * El frontend nunca ve el token ni la URL base: habla con el backend nativo de
 * Tauri (`api_request`), que obtiene el token del pipe de bootstrap y hace la
 * petición HTTP (contrato 07). En pruebas se inyecta otro `Transport`.
 */

/** Respuesta cruda de una petición. */
export interface RawResponse {
  status: number;
  body: unknown;
  headers?: Record<string, string>;
}

/** Petición que el backend nativo ejecuta. Solo rutas `/api/v1/...`. */
export interface RawRequest {
  method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  path: string;
  body?: unknown;
  headers?: Record<string, string>;
}

export type Transport = (request: RawRequest) => Promise<RawResponse>;

/** Error con la forma del contrato 07 (o un fallo de transporte). */
export class ApiError extends Error {
  readonly code: string;
  readonly status: number;
  readonly retryable: boolean;
  readonly correlationId: string | null;
  readonly reason: string | null;

  constructor(code: string, message: string, status: number, retryable: boolean, correlationId: string | null, reason: string | null) {
    super(message);
    this.name = "ApiError";
    this.code = code;
    this.status = status;
    this.retryable = retryable;
    this.correlationId = correlationId;
    this.reason = reason;
  }

  /** El daemon no responde (pipe o HTTP inalcanzables). */
  get isOffline(): boolean {
    return this.code === "DAEMON_UNREACHABLE";
  }
}

interface ContractError {
  code: string;
  message: string;
  retryable: boolean;
  correlation_id: string;
  details?: { reason?: string };
}

function isContractError(value: unknown): value is ContractError {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  return typeof v.code === "string" && typeof v.message === "string" && typeof v.retryable === "boolean";
}

/** Convierte una respuesta no 2xx en `ApiError`, sin inventar campos. */
export function toApiError(response: RawResponse): ApiError {
  if (isContractError(response.body)) {
    const b = response.body;
    return new ApiError(b.code, b.message, response.status, b.retryable, b.correlation_id ?? null, b.details?.reason ?? null);
  }
  return new ApiError("UNEXPECTED_RESPONSE", `Respuesta inesperada del daemon (HTTP ${response.status})`, response.status, false, null, null);
}

const PATH = /^\/api\/v1\/[A-Za-z0-9._~!$&'()*+,;=:@/%?-]*$/;

/** Rechaza rutas fuera de `/api/v1/` antes de salir del frontend. */
export function validatePath(path: string): void {
  if (!PATH.test(path) || path.includes("..")) {
    throw new ApiError("VALIDATION_FAILED", `Ruta no permitida: ${path}`, 0, false, null, "path_invalid");
  }
}

export class ApiClient {
  private readonly transport: Transport;

  constructor(transport: Transport) {
    this.transport = transport;
  }

  async request<T>(request: RawRequest): Promise<T> {
    return (await this.requestRaw<T>(request)).body;
  }

  /** Devuelve el cuerpo junto con metadatos HTTP permitidos, como ETag. */
  async requestRaw<T>(request: RawRequest): Promise<RawResponse & { body: T }> {
    validatePath(request.path);
    let response: RawResponse;
    try {
      response = await this.transport(request);
    } catch (e) {
      throw new ApiError("DAEMON_UNREACHABLE", `No se pudo contactar con el daemon: ${String(e)}`, 0, true, null, "transport");
    }
    if (response.status === 0) {
      throw new ApiError("DAEMON_UNREACHABLE", "El daemon no responde", 0, true, null, "transport");
    }
    if (response.status < 200 || response.status >= 300) {
      throw toApiError(response);
    }
    return response as RawResponse & { body: T };
  }

  get<T>(path: string): Promise<T> {
    return this.request<T>({ method: "GET", path });
  }
}

/** Transporte real: el comando `api_request` del backend nativo. */
export async function tauriTransport(request: RawRequest): Promise<RawResponse> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<RawResponse>("api_request", { request });
}
