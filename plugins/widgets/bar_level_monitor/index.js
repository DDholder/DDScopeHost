function toFinite(val, fallback) {
  return typeof val === "number" && Number.isFinite(val) ? val : fallback;
}

function clampPct(ratio) {
  if (!Number.isFinite(ratio)) return 0;
  return Math.max(0, Math.min(100, ratio * 100));
}

export function mount(container, _api) {
  const root = document.createElement("div");
  root.className = "ddscope-ext-bar-level";

  const top = document.createElement("div");
  top.className = "ddscope-ext-bar-top";

  const chLabel = document.createElement("span");
  chLabel.className = "ddscope-ext-bar-channel";
  chLabel.style.fontSize = "11px";
  chLabel.style.color = "#94a3b8";

  const valueEl = document.createElement("span");
  valueEl.className = "ddscope-ext-bar-value";
  valueEl.textContent = "--";

  top.append(chLabel, valueEl);

  const trackWrap = document.createElement("div");
  trackWrap.className = "ddscope-ext-bar-track-wrap";

  const track = document.createElement("div");
  track.className = "ddscope-ext-bar-track";

  const fill = document.createElement("div");
  fill.className = "ddscope-ext-bar-fill";

  const warnLine = document.createElement("div");
  warnLine.className = "ddscope-ext-bar-warn-line";

  const peakLine = document.createElement("div");
  peakLine.className = "ddscope-ext-bar-peak";

  track.append(fill, warnLine, peakLine);
  trackWrap.append(track);

  const footer = document.createElement("div");
  footer.className = "ddscope-ext-bar-footer";

  const rangeLabel = document.createElement("span");
  const peakLabel = document.createElement("span");
  const resetBtn = document.createElement("button");
  resetBtn.type = "button";
  resetBtn.className = "ddscope-ext-bar-reset-btn";

  let peakValue = -Infinity;
  let lastChannelId = null;

  resetBtn.addEventListener("click", () => {
    peakValue = -Infinity;
    peakLine.style.display = "none";
    peakLabel.textContent = "Peak: --";
  });

  footer.append(rangeLabel, peakLabel, resetBtn);
  root.append(top, trackWrap, footer);
  container.replaceChildren(root);

  const renderState = (state) => {
    const isZh = state.locale === "zh-CN";
    const cfg = state.config || {};
    const min = toFinite(cfg.min, -25);
    const maxRaw = toFinite(cfg.max, 25);
    const max = maxRaw > min ? maxRaw : min + 1;
    const warn = toFinite(cfg.warnThreshold, 18);
    const showPeak = cfg.showPeakHold !== false;

    resetBtn.textContent = isZh ? "峰值归零" : "Reset Peak";

    const ch = state.primaryChannel;
    const curChId = ch ? ch.channel_id : null;
    if (curChId !== lastChannelId) {
      lastChannelId = curChId;
      peakValue = -Infinity;
    }

    const unit = ch && ch.unit ? ` ${ch.unit}` : "";
    chLabel.textContent = ch ? ch.full_path : isZh ? "未绑定通道" : "Unbound channel";
    rangeLabel.textContent = `[${min}, ${max}]`;

    const warnRatio = (warn - min) / (max - min);
    if (warnRatio >= 0 && warnRatio <= 1) {
      warnLine.style.display = "block";
      warnLine.style.left = `${clampPct(warnRatio)}%`;
    } else {
      warnLine.style.display = "none";
    }

    if (typeof state.value === "number" && Number.isFinite(state.value)) {
      const val = state.value;
      if (val > peakValue) {
        peakValue = val;
      }
      const fillPct = clampPct((val - min) / (max - min));
      const isWarn = val >= warn;

      valueEl.textContent = `${val.toFixed(3)}${unit}`;
      valueEl.classList.toggle("is-warn", isWarn);
      fill.classList.toggle("is-warn", isWarn);
      fill.style.width = `${fillPct}%`;

      if (showPeak && Number.isFinite(peakValue)) {
        const peakPct = clampPct((peakValue - min) / (max - min));
        peakLine.style.display = "block";
        peakLine.style.left = `calc(${peakPct}% - 1px)`;
        peakLabel.textContent = `${isZh ? "峰值" : "Peak"}: ${peakValue.toFixed(2)}${unit}`;
      } else {
        peakLine.style.display = "none";
        peakLabel.textContent = "";
      }
    } else {
      valueEl.textContent = `--${unit}`;
      valueEl.classList.remove("is-warn");
      fill.classList.remove("is-warn");
      fill.style.width = "0%";
      peakLine.style.display = "none";
      peakLabel.textContent = showPeak ? `${isZh ? "峰值" : "Peak"}: --` : "";
    }
  };

  return {
    update(state) {
      renderState(state);
    },
    unmount() {
      root.remove();
    },
  };
}
