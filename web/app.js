import {loadingStatus} from "/bootstrap.js";
try {
  if (!crossOriginIsolated) throw new Error("Browser isolation headers are required.");
  if (!navigator.gpu) throw new Error("Open Horizon in a browser with WebGPU support.");
  loadingStatus("Opening Horizon…", 0);
  const {default: init} = await import("/pkg/horizon_client.js");
  await init();
} catch (error) { loadingStatus("Unable to start Horizon: " + (error?.message ?? error), 0); }
