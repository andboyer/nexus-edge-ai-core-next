// Storage + Delivery API wrappers (Phase 5).

import { api } from "@/api/client";
import type {
  ColdReplicaState,
  DeliverySettings,
  OAuthProvider,
  OAuthStartReq,
  OAuthStartResp,
  OAuthStatusResp,
  PutAdminDeliveryReq,
  PutBackendReq,
  PutColdReq,
  PutRuleDeliveryReq,
  RuleDeliveryResp,
  SinksHealthResp,
  StorageBackendOut,
  StorageResponse,
  UsbPreferredOut,
} from "@/api/types";

// --- Storage ---------------------------------------------------------------

export function getStorage() {
  return api.get<StorageResponse>("/v1/storage");
}

export function putBackend(handle: string, req: PutBackendReq) {
  return api.put<StorageBackendOut>(
    `/v1/admin/storage/backends/${encodeURIComponent(handle)}`,
    req,
  );
}

export function deleteBackend(handle: string) {
  return api.delete<void>(
    `/v1/admin/storage/backends/${encodeURIComponent(handle)}`,
  );
}

export function putColdReplica(req: PutColdReq) {
  return api.put<ColdReplicaState>("/v1/admin/storage/cold", req);
}

export function putUsbPreferred(label: string | null) {
  return api.put<UsbPreferredOut>("/v1/admin/runtime/usb_preferred", { label });
}

// --- OAuth ----------------------------------------------------------------

export function startOAuth(provider: OAuthProvider, req: OAuthStartReq) {
  return api.post<OAuthStartResp>(
    `/v1/admin/oauth/${provider}/start`,
    req,
  );
}

export function getOAuthStatus(state: string) {
  return api.get<OAuthStatusResp>("/v1/admin/oauth/status", {
    query: { state },
  });
}

// --- Delivery -------------------------------------------------------------

export function getDeliverySettings() {
  return api.get<DeliverySettings>("/v1/admin/delivery");
}

export function putDeliverySettings(req: PutAdminDeliveryReq) {
  return api.put<DeliverySettings>("/v1/admin/delivery", req);
}

export function getRuleDelivery(ruleId: string) {
  return api.get<RuleDeliveryResp>(
    `/v1/rules/${encodeURIComponent(ruleId)}/delivery`,
  );
}

export function putRuleDelivery(ruleId: string, req: PutRuleDeliveryReq) {
  return api.put<void>(
    `/v1/rules/${encodeURIComponent(ruleId)}/delivery`,
    req,
  );
}

export function getSinksHealth() {
  return api.get<SinksHealthResp>("/v1/admin/sinks/health");
}
