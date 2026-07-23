import type { DeviceSummary } from "@nuntius/shared";

export type FleetVersionState = "compatible" | "mismatch" | "unknown";

export function fleetVersionState(
  serverVersion: string | undefined,
  devices: DeviceSummary[] | undefined,
): FleetVersionState {
  if (!serverVersion || !devices) return "unknown";
  const activeDevices = devices.filter((device) => device.status !== "revoked");
  if (activeDevices.length === 0) return "unknown";
  if (
    activeDevices.some(
      (device) =>
        device.versionCompatibility === "mismatch" ||
        (device.agentVersion !== null && device.agentVersion !== serverVersion),
    )
  ) {
    return "mismatch";
  }
  return activeDevices.every(
    (device) =>
      device.versionCompatibility === "compatible" &&
      device.agentVersion === serverVersion,
  )
    ? "compatible"
    : "unknown";
}
