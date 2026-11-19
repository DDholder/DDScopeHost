import type { ScopeRenderBackend } from "./backend.ts";
import {
  BYTES_PER_VERTEX,
  FLOATS_PER_VERTEX,
  type BuiltScopeGeometry,
} from "./geometry.ts";
import type { MsoLayout } from "./layout.ts";
import type { PersistenceRenderState } from "./persistence.ts";

const MSO_WGSL = /* wgsl */ `
struct Uniforms {
  viewport: vec2<f32>,
  _pad: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> u: Uniforms;

struct VsIn {
  @location(0) posPx: vec2<f32>,
  @location(1) color: vec4<f32>,
  @location(2) uvSize: vec4<f32>,
  @location(3) kind: vec2<f32>,
};

struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) color: vec4<f32>,
  @location(1) uvSize: vec4<f32>,
  @location(2) kind: vec2<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
  var out: VsOut;
  let ndcX = (in.posPx.x / max(1.0, u.viewport.x)) * 2.0 - 1.0;
  let ndcY = 1.0 - (in.posPx.y / max(1.0, u.viewport.y)) * 2.0;
  out.pos = vec4<f32>(ndcX, ndcY, 0.0, 1.0);
  out.color = in.color;
  out.uvSize = in.uvSize;
  out.kind = in.kind;
  return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  let mode = i32(round(in.kind.x));
  var col = in.color;

  if (mode == 1) {
    let feather = 1.0 - smoothstep(0.32, 1.0, abs(in.uvSize.x));
    col.a = col.a * feather;
  } else if (mode == 2) {
    let dpr = max(1.0, in.kind.y);
    let phase = ((in.uvSize.x + in.uvSize.y) / dpr);
    let mod6 = phase - floor(phase / 6.0) * 6.0;
    let stripe = select(0.28, 0.92, mod6 < 2.5);
    col.a = col.a * stripe;
  } else if (mode == 3) {
    let rad = max(1.0, in.kind.y);
    let p = abs(in.uvSize.xy) - (in.uvSize.zw - vec2<f32>(rad, rad));
    let dist = length(max(p, vec2<f32>(0.0))) + min(max(p.x, p.y), 0.0) - rad;
    let mask = 1.0 - smoothstep(-0.75, 0.75, dist);
    let rim = smoothstep(-2.0, -0.5, dist) * 0.16;
    col = vec4<f32>(min(vec3<f32>(1.0), col.rgb + vec3<f32>(rim)), col.a * mask);
  }

  if (col.a <= 0.002) {
    discard;
  }
  return col;
}

@fragment
fn fs_deposit(in: VsOut) -> @location(0) vec4<f32> {
  let mode = i32(round(in.kind.x));
  var alpha = in.color.a;
  if (mode == 1) {
    let feather = 1.0 - smoothstep(0.32, 1.0, abs(in.uvSize.x));
    alpha = alpha * feather;
  }
  let w = alpha * 0.26;
  if (w <= 0.0005) {
    discard;
  }
  return vec4<f32>(in.color.rgb * w, w);
}
`;

const PERSIST_FULLSCREEN_WGSL = /* wgsl */ `
struct PersistParams {
  // x: decayFactor, y: colorMode (0=phosphor, 1=thermal), z: densityScale, w: depositBoost
  params: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> p: PersistParams;
@group(0) @binding(1)
var persistSampler: sampler;
@group(0) @binding(2)
var persistTex: texture_2d<f32>;

struct FsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vid: u32) -> FsOut {
  var pos = vec2<f32>(0.0, 0.0);
  if (vid == 0u) {
    pos = vec2<f32>(-1.0, -1.0);
  } else if (vid == 1u) {
    pos = vec2<f32>(3.0, -1.0);
  } else {
    pos = vec2<f32>(-1.0, 3.0);
  }
  var out: FsOut;
  out.pos = vec4<f32>(pos, 0.0, 1.0);
  out.uv = vec2<f32>((pos.x + 1.0) * 0.5, (1.0 - pos.y) * 0.5);
  return out;
}

@fragment
fn fs_decay(in: FsOut) -> @location(0) vec4<f32> {
  let prev = textureSampleLevel(persistTex, persistSampler, in.uv, 0.0);
  var decayed = prev * p.params.x;
  if (decayed.a < 0.0005) {
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
  }
  if (decayed.a > 64.0) {
    decayed = decayed * (64.0 / decayed.a);
  }
  return decayed;
}

fn sample_thermal_lut(t_in: f32) -> vec3<f32> {
  let t = clamp(t_in, 0.0, 1.0);
  let c0 = vec3<f32>(0.114, 0.306, 0.847); // #1d4ed8
  let c1 = vec3<f32>(0.024, 0.714, 0.831); // #06b6d4
  let c2 = vec3<f32>(0.133, 0.773, 0.369); // #22c55e
  let c3 = vec3<f32>(0.980, 0.800, 0.082); // #facc15
  let c4 = vec3<f32>(0.937, 0.267, 0.267); // #ef4444
  let c5 = vec3<f32>(1.000, 0.969, 0.929); // #fff7ed

  if (t <= 0.25) {
    return mix(c0, c1, t / 0.25);
  } else if (t <= 0.50) {
    return mix(c1, c2, (t - 0.25) / 0.25);
  } else if (t <= 0.75) {
    return mix(c2, c3, (t - 0.50) / 0.25);
  } else if (t <= 0.90) {
    return mix(c3, c4, (t - 0.75) / 0.15);
  }
  return mix(c4, c5, (t - 0.90) / 0.10);
}

@fragment
fn fs_composite(in: FsOut) -> @location(0) vec4<f32> {
  let raw = textureSampleLevel(persistTex, persistSampler, in.uv, 0.0);
  let density = raw.a * p.params.z;
  if (density <= 0.002) {
    discard;
  }
  let norm = 1.0 - exp(-density * 1.15);

  if (p.params.y < 0.5) {
    let baseRgb = clamp(raw.rgb / max(raw.a, 0.0001), vec3<f32>(0.0), vec3<f32>(1.0));
    let hotCore = pow(norm, 2.2) * 0.45;
    let outRgb = mix(baseRgb, vec3<f32>(1.0), hotCore);
    let outAlpha = clamp(pow(norm, 0.72) * 0.95, 0.0, 0.95);
    return vec4<f32>(outRgb, outAlpha);
  }

  let heat = clamp(log2(1.0 + density * 2.5) / 3.2, 0.0, 1.0);
  let outRgb = sample_thermal_lut(heat);
  let outAlpha = clamp(pow(norm, 0.62) * 0.96, 0.0, 0.96);
  return vec4<f32>(outRgb, outAlpha);
}
`;

export class WebGpuScopeBackend implements ScopeRenderBackend {
  readonly kind = "webgpu" as const;

  private readonly device: GPUDevice;
  private readonly context: GPUCanvasContext;
  private readonly format: GPUTextureFormat;
  private readonly msoModule: GPUShaderModule;
  private readonly uniformPipelineLayout: GPUPipelineLayout;
  private readonly pipeline: GPURenderPipeline;
  private readonly uniformBuffer: GPUBuffer;
  private readonly bindGroup: GPUBindGroup;

  private vertexBuffer: GPUBuffer;
  private vertexBufferBytes = 0;
  private readonly uniformScratch = new Float32Array(4);
  private configuredWidth = 0;
  private configuredHeight = 0;
  private disposed = false;

  // Lazy persistence resources
  private persistPipelinesInitialized = false;
  private persistBindGroupLayout: GPUBindGroupLayout | null = null;
  private persistDecayPipeline: GPURenderPipeline | null = null;
  private persistDepositPipeline: GPURenderPipeline | null = null;
  private persistCompositePipeline: GPURenderPipeline | null = null;
  private persistSampler: GPUSampler | null = null;
  private persistParamsBuffer: GPUBuffer | null = null;
  private readonly persistParamsScratch = new Float32Array(4);

  private persistTexA: GPUTexture | null = null;
  private persistTexB: GPUTexture | null = null;
  private persistViewA: GPUTextureView | null = null;
  private persistViewB: GPUTextureView | null = null;
  private persistBindGroupA: GPUBindGroup | null = null;
  private persistBindGroupB: GPUBindGroup | null = null;
  private persistReadIndex: 0 | 1 = 0;
  private persistWidth = 0;
  private persistHeight = 0;
  private persistNeedsClear = true;

  constructor(device: GPUDevice, context: GPUCanvasContext, format: GPUTextureFormat) {
    this.device = device;
    this.context = context;
    this.format = format;

    const module = device.createShaderModule({ code: MSO_WGSL });
    this.msoModule = module;

    const uniformBindGroupLayout = device.createBindGroupLayout({
      entries: [
        {
          binding: 0,
          visibility: GPUShaderStage.VERTEX,
          buffer: { type: "uniform" },
        },
      ],
    });
    this.uniformPipelineLayout = device.createPipelineLayout({
      bindGroupLayouts: [uniformBindGroupLayout],
    });

    this.pipeline = device.createRenderPipeline({
      layout: this.uniformPipelineLayout,
      vertex: {
        module,
        entryPoint: "vs_main",
        buffers: [
          {
            arrayStride: BYTES_PER_VERTEX,
            attributes: [
              { shaderLocation: 0, offset: 0, format: "float32x2" },
              { shaderLocation: 1, offset: 8, format: "float32x4" },
              { shaderLocation: 2, offset: 24, format: "float32x4" },
              { shaderLocation: 3, offset: 40, format: "float32x2" },
            ],
          },
        ],
      },
      fragment: {
        module,
        entryPoint: "fs_main",
        targets: [
          {
            format,
            blend: {
              color: {
                srcFactor: "src-alpha",
                dstFactor: "one-minus-src-alpha",
                operation: "add",
              },
              alpha: {
                srcFactor: "one",
                dstFactor: "one-minus-src-alpha",
                operation: "add",
              },
            },
          },
        ],
      },
      primitive: {
        topology: "triangle-list",
      },
    });

    this.uniformBuffer = device.createBuffer({
      size: 16,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });

    this.bindGroup = device.createBindGroup({
      layout: uniformBindGroupLayout,
      entries: [{ binding: 0, resource: { buffer: this.uniformBuffer } }],
    });

    this.vertexBufferBytes = 65_536 * BYTES_PER_VERTEX;
    this.vertexBuffer = device.createBuffer({
      size: this.vertexBufferBytes,
      usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
    });
  }

  private ensurePersistencePipelines(): void {
    if (this.persistPipelinesInitialized) return;
    this.persistPipelinesInitialized = true;

    const device = this.device;
    const fsModule = device.createShaderModule({ code: PERSIST_FULLSCREEN_WGSL });

    const bgl = device.createBindGroupLayout({
      entries: [
        {
          binding: 0,
          visibility: GPUShaderStage.FRAGMENT,
          buffer: { type: "uniform" },
        },
        {
          binding: 1,
          visibility: GPUShaderStage.FRAGMENT,
          sampler: { type: "filtering" },
        },
        {
          binding: 2,
          visibility: GPUShaderStage.FRAGMENT,
          texture: { sampleType: "float" },
        },
      ],
    });
    this.persistBindGroupLayout = bgl;

    const persistLayout = device.createPipelineLayout({
      bindGroupLayouts: [bgl],
    });

    this.persistDecayPipeline = device.createRenderPipeline({
      layout: persistLayout,
      vertex: {
        module: fsModule,
        entryPoint: "vs_fullscreen",
      },
      fragment: {
        module: fsModule,
        entryPoint: "fs_decay",
        targets: [{ format: "rgba16float" }],
      },
      primitive: {
        topology: "triangle-list",
      },
    });

    this.persistDepositPipeline = device.createRenderPipeline({
      layout: this.uniformPipelineLayout,
      vertex: {
        module: this.msoModule,
        entryPoint: "vs_main",
        buffers: [
          {
            arrayStride: BYTES_PER_VERTEX,
            attributes: [
              { shaderLocation: 0, offset: 0, format: "float32x2" },
              { shaderLocation: 1, offset: 8, format: "float32x4" },
              { shaderLocation: 2, offset: 24, format: "float32x4" },
              { shaderLocation: 3, offset: 40, format: "float32x2" },
            ],
          },
        ],
      },
      fragment: {
        module: this.msoModule,
        entryPoint: "fs_deposit",
        targets: [
          {
            format: "rgba16float",
            blend: {
              color: {
                srcFactor: "one",
                dstFactor: "one",
                operation: "add",
              },
              alpha: {
                srcFactor: "one",
                dstFactor: "one",
                operation: "add",
              },
            },
          },
        ],
      },
      primitive: {
        topology: "triangle-list",
      },
    });

    this.persistCompositePipeline = device.createRenderPipeline({
      layout: persistLayout,
      vertex: {
        module: fsModule,
        entryPoint: "vs_fullscreen",
      },
      fragment: {
        module: fsModule,
        entryPoint: "fs_composite",
        targets: [
          {
            format: this.format,
            blend: {
              color: {
                srcFactor: "src-alpha",
                dstFactor: "one-minus-src-alpha",
                operation: "add",
              },
              alpha: {
                srcFactor: "one",
                dstFactor: "one-minus-src-alpha",
                operation: "add",
              },
            },
          },
        ],
      },
      primitive: {
        topology: "triangle-list",
      },
    });

    this.persistSampler = device.createSampler({
      magFilter: "linear",
      minFilter: "linear",
    });

    this.persistParamsBuffer = device.createBuffer({
      size: 16,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
  }

  private destroyPersistenceTextures(): void {
    this.persistTexA?.destroy();
    this.persistTexB?.destroy();
    this.persistTexA = null;
    this.persistTexB = null;
    this.persistViewA = null;
    this.persistViewB = null;
    this.persistBindGroupA = null;
    this.persistBindGroupB = null;
    this.persistWidth = 0;
    this.persistHeight = 0;
    this.persistReadIndex = 0;
    this.persistNeedsClear = true;
  }

  private ensurePersistenceTextures(physicalWidth: number, physicalHeight: number): boolean {
    this.ensurePersistencePipelines();
    if (
      this.persistTexA &&
      this.persistTexB &&
      this.persistWidth === physicalWidth &&
      this.persistHeight === physicalHeight
    ) {
      return false;
    }

    this.destroyPersistenceTextures();

    const usage =
      GPUTextureUsage.RENDER_ATTACHMENT |
      GPUTextureUsage.TEXTURE_BINDING |
      GPUTextureUsage.COPY_DST;

    this.persistTexA = this.device.createTexture({
      size: { width: physicalWidth, height: physicalHeight },
      format: "rgba16float",
      usage,
    });
    this.persistTexB = this.device.createTexture({
      size: { width: physicalWidth, height: physicalHeight },
      format: "rgba16float",
      usage,
    });
    this.persistViewA = this.persistTexA.createView();
    this.persistViewB = this.persistTexB.createView();

    const bgl = this.persistBindGroupLayout!;
    const sampler = this.persistSampler!;
    const paramsBuf = this.persistParamsBuffer!;

    this.persistBindGroupA = this.device.createBindGroup({
      layout: bgl,
      entries: [
        { binding: 0, resource: { buffer: paramsBuf } },
        { binding: 1, resource: sampler },
        { binding: 2, resource: this.persistViewA },
      ],
    });
    this.persistBindGroupB = this.device.createBindGroup({
      layout: bgl,
      entries: [
        { binding: 0, resource: { buffer: paramsBuf } },
        { binding: 1, resource: sampler },
        { binding: 2, resource: this.persistViewB },
      ],
    });

    this.persistWidth = physicalWidth;
    this.persistHeight = physicalHeight;
    this.persistReadIndex = 0;
    this.persistNeedsClear = true;
    return true;
  }

  resize(physicalWidth: number, physicalHeight: number): void {
    if (this.disposed || physicalWidth < 2 || physicalHeight < 2) return;
    if (
      this.configuredWidth === physicalWidth &&
      this.configuredHeight === physicalHeight
    ) {
      return;
    }
    this.context.configure({
      device: this.device,
      format: this.format,
      alphaMode: "premultiplied",
    });
    this.configuredWidth = physicalWidth;
    this.configuredHeight = physicalHeight;
  }

  private ensureVertexCapacity(requiredBytes: number): void {
    if (requiredBytes <= this.vertexBufferBytes) return;
    let nextBytes = this.vertexBufferBytes * 2;
    while (nextBytes < requiredBytes) {
      nextBytes *= 2;
    }
    this.vertexBuffer.destroy();
    this.vertexBufferBytes = nextBytes;
    this.vertexBuffer = this.device.createBuffer({
      size: nextBytes,
      usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
    });
  }

  render(
    physicalWidth: number,
    physicalHeight: number,
    dpr: number,
    layout: MsoLayout,
    geometry: BuiltScopeGeometry,
    persistence?: PersistenceRenderState,
  ): void {
    if (this.disposed || physicalWidth < 2 || physicalHeight < 2) return;
    this.resize(physicalWidth, physicalHeight);

    const texture = this.context.getCurrentTexture();
    const view = texture.createView();

    this.uniformScratch[0] = physicalWidth;
    this.uniformScratch[1] = physicalHeight;
    this.device.queue.writeBuffer(this.uniformBuffer, 0, this.uniformScratch);

    const neededBytes = geometry.totalVertexCount * BYTES_PER_VERTEX;
    if (neededBytes > 0) {
      this.ensureVertexCapacity(neededBytes);
      const floatLen = geometry.totalVertexCount * FLOATS_PER_VERTEX;
      this.device.queue.writeBuffer(
        this.vertexBuffer,
        0,
        geometry.vertices.buffer,
        geometry.vertices.byteOffset,
        floatLen * 4,
      );
    }

    const sx = Math.max(0, Math.min(physicalWidth - 1, Math.floor(layout.plotLeft * dpr)));
    const sy = Math.max(0, Math.min(physicalHeight - 1, Math.floor(layout.plotTop * dpr)));
    const sw = Math.max(1, Math.min(physicalWidth - sx, Math.ceil(layout.plotWidth * dpr)));
    const sh = Math.max(1, Math.min(physicalHeight - sy, Math.ceil(layout.plotHeight * dpr)));

    if (!persistence || persistence.mode === "off") {
      this.persistNeedsClear = true;

      const encoder = this.device.createCommandEncoder();
      const pass = encoder.beginRenderPass({
        colorAttachments: [
          {
            view,
            clearValue: { r: 0.027, g: 0.039, b: 0.067, a: 1.0 },
            loadOp: "clear",
            storeOp: "store",
          },
        ],
      });

      if (geometry.totalVertexCount > 0) {
        pass.setPipeline(this.pipeline);
        pass.setBindGroup(0, this.bindGroup);
        pass.setVertexBuffer(0, this.vertexBuffer);

        if (geometry.gridVertexCount > 0) {
          pass.setScissorRect(0, 0, physicalWidth, physicalHeight);
          pass.draw(geometry.gridVertexCount, 1, 0, 0);
        }

        if (geometry.signalVertexCount > 0) {
          pass.setScissorRect(sx, sy, sw, sh);
          pass.draw(geometry.signalVertexCount, 1, geometry.gridVertexCount, 0);
        }
      }

      pass.end();
      this.device.queue.submit([encoder.finish()]);
      return;
    }

    const allocated = this.ensurePersistenceTextures(physicalWidth, physicalHeight);
    const doClear = allocated || persistence.clear || this.persistNeedsClear;

    this.persistParamsScratch[0] = persistence.decayFactor;
    this.persistParamsScratch[1] = persistence.colorMode === "thermal" ? 1 : 0;
    this.persistParamsScratch[2] = 1.0;
    this.persistParamsScratch[3] = 0.26;
    this.device.queue.writeBuffer(this.persistParamsBuffer!, 0, this.persistParamsScratch);

    const ax = Math.max(
      0,
      Math.min(physicalWidth - 1, Math.floor(layout.analogRegion.x * dpr)),
    );
    const ay = Math.max(
      0,
      Math.min(physicalHeight - 1, Math.floor(layout.analogRegion.y * dpr)),
    );
    const aw = Math.max(
      1,
      Math.min(physicalWidth - ax, Math.ceil(layout.analogRegion.width * dpr)),
    );
    const ah = Math.max(
      1,
      Math.min(physicalHeight - ay, Math.ceil(layout.analogRegion.height * dpr)),
    );

    const needsPass1 =
      doClear ||
      persistence.decayFactor < 0.99999 ||
      (persistence.depositCurrentFrame && geometry.analogVertexCount > 0);

    const encoder = this.device.createCommandEncoder();

    if (needsPass1) {
      const srcBindGroup =
        this.persistReadIndex === 0 ? this.persistBindGroupA! : this.persistBindGroupB!;
      const dstView =
        this.persistReadIndex === 0 ? this.persistViewB! : this.persistViewA!;

      const pass1 = encoder.beginRenderPass({
        colorAttachments: [
          {
            view: dstView,
            clearValue: { r: 0, g: 0, b: 0, a: 0 },
            loadOp: "clear",
            storeOp: "store",
          },
        ],
      });

      if (!doClear) {
        pass1.setPipeline(this.persistDecayPipeline!);
        pass1.setBindGroup(0, srcBindGroup);
        pass1.setScissorRect(0, 0, physicalWidth, physicalHeight);
        pass1.draw(3, 1, 0, 0);
      }

      if (persistence.depositCurrentFrame && geometry.analogVertexCount > 0) {
        pass1.setPipeline(this.persistDepositPipeline!);
        pass1.setBindGroup(0, this.bindGroup);
        pass1.setVertexBuffer(0, this.vertexBuffer);
        pass1.setScissorRect(ax, ay, aw, ah);
        pass1.draw(geometry.analogVertexCount, 1, geometry.gridVertexCount, 0);
      }

      pass1.end();
      this.persistReadIndex = (1 - this.persistReadIndex) as 0 | 1;
      this.persistNeedsClear = false;
    }

    const currentPersistBindGroup =
      this.persistReadIndex === 0 ? this.persistBindGroupA! : this.persistBindGroupB!;

    const pass2 = encoder.beginRenderPass({
      colorAttachments: [
        {
          view,
          clearValue: { r: 0.027, g: 0.039, b: 0.067, a: 1.0 },
          loadOp: "clear",
          storeOp: "store",
        },
      ],
    });

    if (geometry.gridVertexCount > 0) {
      pass2.setPipeline(this.pipeline);
      pass2.setBindGroup(0, this.bindGroup);
      pass2.setVertexBuffer(0, this.vertexBuffer);
      pass2.setScissorRect(0, 0, physicalWidth, physicalHeight);
      pass2.draw(geometry.gridVertexCount, 1, 0, 0);
    }

    pass2.setPipeline(this.persistCompositePipeline!);
    pass2.setBindGroup(0, currentPersistBindGroup);
    pass2.setScissorRect(ax, ay, aw, ah);
    pass2.draw(3, 1, 0, 0);

    if (
      persistence.colorMode === "phosphor" &&
      geometry.analogVertexCount > 0
    ) {
      pass2.setPipeline(this.pipeline);
      pass2.setBindGroup(0, this.bindGroup);
      pass2.setVertexBuffer(0, this.vertexBuffer);
      pass2.setScissorRect(ax, ay, aw, ah);
      pass2.draw(geometry.analogVertexCount, 1, geometry.gridVertexCount, 0);
    }

    if (geometry.overlaySignalVertexCount > 0) {
      pass2.setPipeline(this.pipeline);
      pass2.setBindGroup(0, this.bindGroup);
      pass2.setVertexBuffer(0, this.vertexBuffer);
      pass2.setScissorRect(sx, sy, sw, sh);
      pass2.draw(
        geometry.overlaySignalVertexCount,
        1,
        geometry.gridVertexCount + geometry.analogVertexCount,
        0,
      );
    }

    pass2.end();
    this.device.queue.submit([encoder.finish()]);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.destroyPersistenceTextures();
    this.persistParamsBuffer?.destroy();
    this.persistParamsBuffer = null;
    this.vertexBuffer.destroy();
    this.uniformBuffer.destroy();
  }
}
