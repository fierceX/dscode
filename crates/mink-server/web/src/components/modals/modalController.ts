// Modal 管理：动态挂载 ModalShell（Teleport + 原生 dialog）。

import { createApp, h, type Component } from "vue";
import ModalShell from "./ModalShell.vue";

let host: HTMLElement | null = null;
let app: ReturnType<typeof createApp> | null = null;

export function openModal(content: Component, contentProps: Record<string, unknown>, title: string): void {
  closeModal();
  host = document.createElement("div");
  document.body.appendChild(host);
  app = createApp({
    render: () =>
      h(ModalShell, {
        title,
        content,
        contentProps,
        onClose: () => closeModal(),
      }),
  });
  app.mount(host);
}

export function closeModal(): void {
  if (app) {
    app.unmount();
    app = null;
  }
  if (host) {
    host.remove();
    host = null;
  }
}
