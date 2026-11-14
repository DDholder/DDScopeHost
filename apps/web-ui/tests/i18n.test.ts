import test from "node:test";
import assert from "node:assert/strict";
import {
  EN_MESSAGES,
  ZH_CN_MESSAGES,
  detectInitialLocale,
  getDefaultScopeTitle,
  isSupportedLocale,
  persistLocale,
  translate,
  translateDefaultScopeTitle,
  type MessageKey,
} from "../src/i18n/index.ts";

function extractPlaceholders(template: string): string[] {
  const matches = template.match(/\{[a-zA-Z0-9_]+\}/g) ?? [];
  return [...matches].sort();
}

test("i18n dictionaries have identical keys, non-empty strings, and matching placeholders", () => {
  const enKeys = Object.keys(EN_MESSAGES).sort() as MessageKey[];
  const zhKeys = Object.keys(ZH_CN_MESSAGES).sort() as MessageKey[];

  assert.deepEqual(zhKeys, enKeys);
  assert.ok(enKeys.length > 90);

  for (const key of enKeys) {
    const enVal = EN_MESSAGES[key];
    const zhVal = ZH_CN_MESSAGES[key];
    assert.ok(enVal.trim().length > 0, `EN translation empty for ${key}`);
    assert.ok(zhVal.trim().length > 0, `zh-CN translation empty for ${key}`);
    assert.deepEqual(
      extractPlaceholders(zhVal),
      extractPlaceholders(enVal),
      `Placeholder mismatch for key '${key}'`,
    );
  }
});

test("translate performs parameter interpolation for both locales", () => {
  assert.equal(translate("en", "topbar.seq", { seq: 42 }), "seq #42");
  assert.equal(translate("zh-CN", "topbar.seq", { seq: 42 }), "帧 #42");
  assert.equal(translate("en", "sidebar.bindTitle", { id: 3 }), "Bind to Scope 3");
  assert.equal(translate("zh-CN", "sidebar.bindTitle", { id: 3 }), "绑定到示波器 3");
});

test("default scope titles round-trip across zh-CN and en while preserving custom titles", () => {
  const s1En = getDefaultScopeTitle("scope1", "en");
  const s1Zh = getDefaultScopeTitle("scope1", "zh-CN");
  assert.equal(translateDefaultScopeTitle(s1En, "zh-CN"), s1Zh);
  assert.equal(translateDefaultScopeTitle(s1Zh, "en"), s1En);

  const s2En = getDefaultScopeTitle("scope2", "en");
  const s2Zh = getDefaultScopeTitle("scope2", "zh-CN");
  assert.equal(translateDefaultScopeTitle(s2En, "zh-CN"), s2Zh);
  assert.equal(translateDefaultScopeTitle(s2Zh, "en"), s2En);

  const s5En = getDefaultScopeTitle("generic", "en", 5);
  const s5Zh = getDefaultScopeTitle("generic", "zh-CN", 5);
  assert.equal(translateDefaultScopeTitle(s5En, "zh-CN"), s5Zh);
  assert.equal(translateDefaultScopeTitle(s5Zh, "en"), s5En);

  assert.equal(translateDefaultScopeTitle("Custom Inverter Bus", "zh-CN"), "Custom Inverter Bus");
  assert.equal(translateDefaultScopeTitle("自定义逆变器通道", "en"), "自定义逆变器通道");
});

test("locale validation and non-DOM environment fallback", () => {
  assert.equal(isSupportedLocale("zh-CN"), true);
  assert.equal(isSupportedLocale("en"), true);
  assert.equal(isSupportedLocale("fr"), false);

  const initial = detectInitialLocale();
  assert.equal(isSupportedLocale(initial), true);
  assert.doesNotThrow(() => persistLocale("zh-CN"));
});
