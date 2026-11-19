import type { ChannelNodeDto_Serialize } from "../bridge/bindings.ts";

export interface ChannelBindSelectProps {
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  placeholder: string;
  selectedChannelId?: number | null;
  title?: string;
  excludeChannelIds?: ReadonlySet<number>;
  className?: string;
  dataTestId?: string;
  onBindChannel?: (channelId: number) => void;
}

/**
 * Channel-picker dropdown used in widget title bars so users can bind,
 * switch, or append channels in place without consuming widget body space.
 */
export function ChannelBindSelect({
  channelsById,
  placeholder,
  selectedChannelId,
  title,
  excludeChannelIds,
  className,
  dataTestId,
  onBindChannel,
}: ChannelBindSelectProps) {
  if (!onBindChannel) return null;

  const isBound = selectedChannelId !== undefined && selectedChannelId !== null;
  const options = Object.values(channelsById)
    .filter((ch) => !excludeChannelIds?.has(ch.channel_id))
    .sort((a, b) => a.full_path.localeCompare(b.full_path));

  if (!isBound && options.length === 0) return null;

  const hasSelectedOption =
    isBound && options.some((ch) => ch.channel_id === selectedChannelId);

  return (
    <select
      className={className ?? "widget-unbound-select"}
      data-testid={dataTestId}
      title={title ?? placeholder}
      value={isBound ? String(selectedChannelId) : ""}
      onPointerDown={(ev) => ev.stopPropagation()}
      onClick={(ev) => ev.stopPropagation()}
      onChange={(ev) => {
        const id = Number.parseInt(ev.target.value, 10);
        if (Number.isFinite(id)) {
          onBindChannel(id);
        }
      }}
    >
      {!isBound && (
        <option value="" disabled>
          {placeholder}
        </option>
      )}
      {isBound && !hasSelectedOption && (
        <option value={selectedChannelId}>ch{selectedChannelId}</option>
      )}
      {options.map((ch) => (
        <option key={ch.channel_id} value={ch.channel_id}>
          {ch.full_path}
          {ch.unit ? ` (${ch.unit})` : ""}
        </option>
      ))}
    </select>
  );
}
