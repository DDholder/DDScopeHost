import type { ChannelNodeDto_Serialize } from "../bridge/bindings.ts";

export interface ChannelBindSelectProps {
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  placeholder: string;
  onBindChannel?: (channelId: number) => void;
}

/**
 * Fallback channel-picker dropdown shown inside unbound widgets when drag-and-drop
 * is inconvenient or unsupported.
 */
export function ChannelBindSelect({
  channelsById,
  placeholder,
  onBindChannel,
}: ChannelBindSelectProps) {
  if (!onBindChannel) return null;
  const options = Object.values(channelsById).sort((a, b) =>
    a.full_path.localeCompare(b.full_path),
  );
  if (options.length === 0) return null;

  return (
    <select
      className="widget-unbound-select"
      defaultValue=""
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
