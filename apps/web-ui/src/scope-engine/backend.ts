import type { BuiltScopeGeometry } from "./geometry.ts";
import type { MsoLayout } from "./layout.ts";
import type { PersistenceRenderState } from "./persistence.ts";

export type RenderBackendKind = "webgpu" | "webgl2" | "canvas2d";

export interface ScopeRenderBackend {
  readonly kind: RenderBackendKind;
  resize(physicalWidth: number, physicalHeight: number): void;
  render(
    physicalWidth: number,
    physicalHeight: number,
    dpr: number,
    layout: MsoLayout,
    geometry: BuiltScopeGeometry,
    persistence?: PersistenceRenderState,
  ): void;
  dispose(): void;
}
