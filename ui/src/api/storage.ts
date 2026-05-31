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
  return api.get<StorageResponse>("/storage");
}

export function putBackend(handle: string, req: PutBackendReq) {
  return api.put<StorageBackendOut>(
    `/admin/storage/backends/${encodeURIComponent(handle)}`,
    req,
  );
}

export function deleteBackend(handle: string) {
  return api.delete<void>(
    `/admin/storage/backends/${encodeURIComponent(handle)}`,
  );
}

export function putColdReplica(req: PutColdReq) {
  return api.put<ColdReplicaState>("/admin/storage/cold", req);
}

export function putUsbPreferred(label: string | null) {
  return api.put<UsbPreferredOut>("/admin/runtime/usb_preferred", { label });
}

// --- OAuth ----------------------------------------------------------------

export function startOAuth(provider: OAuthProvider, req: OAuthStartReq) {
  return api.post<OAuthStartResp>(
    `/admin/oauth/${provider}/start`,
    req,
  );
}

export function getOAuthStatus(state: string) {
  return api.get<OAuthStatusResp>("/admin/oauth/status", {
    query: { state },
  });
}

// --- Delivery -------------------------------------------------------------

export function getDeliverySettings() {
  return api.get<DeliverySettings>("/admin/delivery");
}

export function putDeliverySettings(req: PutAdminDeliveryReq) {
  return api.put<DeliverySettings>("/admin/delivery", req);
}

export function getRuleDelivery(ruleId: string) {
  return api.get<RuleDeliveryResp>(
    `/rules/${encodeURIComponent(ruleId)}/delivery`,
  );
}

export function putRuleDelivery(ruleId: string, req: PutRuleDeliveryReq) {
  return api.put<void>(
    `/rules/${encodeURIComponent(ruleId)}/delivery`,
    req,
  );
}

export function getSinksHealth() {
  return api.get<SinksHealthResp>("/admin/sinks/health");
}
