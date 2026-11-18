import type { ChannelNodeDto_Serialize } from "../bridge/bindings.ts";

export interface ChannelBindSelectProps {
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  placeholder: string;
  excludeChannelIds?: ReadonlySet<number>;
  className?: string;
  dataTestId?: string;
  onBindChannel?: (channelId: number) => void;
}

/**
 * Channel-picker dropdown shown inside unbound or multi-channel widgets so
 * users can add channels directly without drag-and-drop.
 */
export function ChannelBindSelect({
  channelsById,
  placeholder,
  excludeChannelIds,
  className,
  dataTestId,
  onBindChannel,
}: ChannelBindSelectProps) {
  if (!onBindChannel) return null;
  const options = Object.values(channelsById)
    .filter((ch) => !excludeChannelIds?.has(ch.channel_id))
    .sort((a, b) => a.full_path.localeCompare(b.full_path));
  if (options.length === 0) return null;

  return (
    <select
      className={className ?? "widget-unbound-select"}
      data-testid={dataTestId}
      value=""
      onPointerDown={(ev) => ev.stopPropagation()}
      onClick={(ev) => ev.stopPropagation()}
      onChange={(ev) => {
        const id = Number.parseInt(ev.target.value, 10);
        if (Number.isFinite(id)) {
          onBindChannel(id);
        }
      }}
    >
      <option value="" disabled>
        {placeholder}
      </option>
      {options.map((ch) => (
        <option key={ch.channel_id} value={ch.channel_id}>
          {ch.full_path}
          {ch.unit ? ` (${ch.unit})` : ""}
        </option>
      ))}
    </select>
  );
}
