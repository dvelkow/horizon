import init, {wasm_thread_entry_point} from "/pkg/horizon_client.js";
self.onmessage = async ({data: [module, memory, work]}) => {
  try {
    await init({module_or_path: module, memory});
    wasm_thread_entry_point(work);
    self.close();
  } catch (error) { setTimeout(() => { throw error; }); }
};
