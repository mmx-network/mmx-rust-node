import { useRef } from 'preact/hooks';
import { Amount, Badge, Icon, IconTile, ListItem, Skeleton } from '../../components';
import { clock, counterpartyFor, memoFor, params, statusFor, type ActivityItem } from '../../../state';
import { formatRelative, formatTime } from '../../../lib/time';
import {
  activityIcon,
  activityTitle,
  counterpartyLine,
  extraAssetCount,
  isUnverified,
  rowAmount,
  STATUS_LABELS,
  statusTone,
} from './present';
import { useSeen } from './useSeen';
import './activity.css';

export interface ActivityRowProps {
  item: ActivityItem;
  onOpen: (key: string) => void;
  /** time: time of day (the list is grouped by day); relative: "5 min ago", "Mar 4". */
  timeMode?: 'time' | 'relative';
  selected?: boolean;
}

/** Locale formatter for the Amount component (exact, trailing zeros trimmed). */

const NEEDS_COUNTERPARTY = new Set(['received', 'sent', 'fee']);

export function ActivityRow({ item, onOpen, timeMode = 'time', selected = false }: ActivityRowProps) {
  const titleRef = useRef<HTMLSpanElement>(null);
  const seen = useSeen(titleRef);
  const status = statusFor(item);
  const icon = activityIcon(item, status);
  const amount = rowAmount(item, status, params.value?.decimals ?? 6);
  const wantsCounterparty = NEEDS_COUNTERPARTY.has(item.kind);
  const cp = wantsCounterparty && (seen || item.local) ? counterpartyFor(item) : undefined;
  const party = counterpartyLine(item, cp);
  const memo = memoFor(item);
  const unverified = isUnverified(item);
  const extra = extraAssetCount(item);
  const unsuccessful = status === 'failed' || status === 'expired';

  const parts: string[] = [];
  if (unverified) parts.push('Unverified token');
  if (party) parts.push(party);
  const loadingParty = wantsCounterparty && cp === undefined && !item.local;
  const subtitle =
    parts.length > 0 || memo || loadingParty ? (
      <>
        {parts.join(' · ')}
        {loadingParty ? (
          <>
            {parts.length > 0 ? ' · ' : ''}
            <Skeleton class="activity-row__skeleton" width="6.5rem" />
          </>
        ) : null}
        {memo ? (
          <>
            {parts.length > 0 || loadingParty ? ' · ' : ''}
            <span class="activity-row__memo">{`\u{201c}${memo}\u{201d}`}</span>
          </>
        ) : null}
      </>
    ) : undefined;

  const when =
    timeMode === 'relative' || item.height === undefined
      ? formatRelative(item.timeMs, clock.value)
      : formatTime(item.timeMs);

  return (
    <ListItem
      class={`activity-row activity-row--${item.kind}`}
      selected={selected}
      onClick={() => onOpen(item.key)}
      leading={<IconTile icon={icon.icon} tone={icon.tone} />}
      title={
        <span class="activity-row__title" ref={titleRef}>
          {activityTitle(item)}
        </span>
      }
      subtitle={subtitle}
      trailing={
        amount ? (
          <span class={`activity-row__amount${unsuccessful ? ' activity-row__amount--void' : ''}`}>
            {unverified ? <Icon name="warning" size="sm" class="activity-row__warn" /> : null}
            <Amount
              value={amount.value}
              decimals={amount.decimals}
              symbol={amount.symbol}
              sign={amount.sign}
              tone={amount.tone}
            />
          </span>
        ) : undefined
      }
      trailingSub={
        <span class="activity-row__meta">
          {status !== 'confirmed' ? (
            <Badge tone={statusTone(status)}>{STATUS_LABELS[status]}</Badge>
          ) : (
            <time dateTime={new Date(item.timeMs).toISOString()}>{when}</time>
          )}
          {extra > 0 ? <span class="activity-row__extra">{`+${extra} more`}</span> : null}
        </span>
      }
    />
  );
}
