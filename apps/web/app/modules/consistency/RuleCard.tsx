// The board's rule card and the archive view's row. Both are presentational:
// every verb is a callback, so the page owns the mutations and the errors.

import type { LucideIcon } from 'lucide-react';
import {
  Archive,
  BookOpen,
  Calendar,
  ChevronDown,
  ChevronUp,
  Clock,
  Dumbbell,
  Flame,
  Moon,
  Pause,
  Pencil,
  Play,
  RotateCcw,
  Target,
} from 'lucide-react';
import type { RuleScore } from '@/app/types';
import { cn } from '@/lib/utils';
import { RuleEvidence } from './RuleEvidence';
import {
  archiveVerdict,
  boardVerdict,
  phaseLabel,
  stripClass,
} from './rules-model';

/** `rules.icon` → glyph. An unknown (or empty) icon falls back to Target. */
const ICONS: Record<string, LucideIcon> = {
  fitness: Dumbbell,
  learning: BookOpen,
  productive: Target,
  sleep: Moon,
  bedtime: Moon,
  wake: Clock,
};

/**
 * The verdict chip's tone. `On track` is the only coloured word; `Behind` is
 * plain foreground on purpose — a behind rule is a reading, not a failure.
 */
const CHIP_TONE: Record<string, string> = {
  'On track': 'bg-primary/10 text-primary',
  Behind: 'bg-muted text-foreground',
};

/** The small pill the header carries — never a PASS/FAIL stamp. */
function VerdictChip({
  word,
  second,
}: {
  word: string;
  second?: string | null;
}) {
  return (
    <span
      className={cn(
        'shrink-0 rounded-full px-2 py-0.5 text-[10px] uppercase tracking-wide',
        CHIP_TONE[word] ?? 'bg-muted text-muted-foreground',
      )}
    >
      {word}
      {second ? (
        <span className="text-muted-foreground/80"> · {second}</span>
      ) : null}
    </span>
  );
}

// ──────────────────────────────────────────
// Shared pieces
// ──────────────────────────────────────────

/** Icon, name, condition, and the verdict chip: the card's toggle button. */
function RuleHeader({
  rule,
  chip,
}: {
  rule: RuleScore;
  chip: React.ReactNode;
}) {
  const Icon = ICONS[rule.icon] ?? Target;
  return (
    <div className="flex items-start gap-3">
      <div className="rounded-lg bg-primary/10 p-2 text-primary">
        <Icon className="h-5 w-5" />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <h3 className="truncate font-heading font-semibold text-foreground">
            {rule.name}
          </h3>
          {chip}
        </div>
        <p className="text-sm text-muted-foreground">{rule.condition}</p>
      </div>
    </div>
  );
}

function Stat({
  icon: Icon,
  tone,
  value,
  label,
}: {
  icon: LucideIcon;
  tone: string;
  value: string;
  label: string;
}) {
  return (
    <div className="text-center">
      <div
        className={cn('mb-0.5 flex items-center justify-center gap-1', tone)}
      >
        <Icon className="h-4 w-4" />
        <span className="text-lg font-bold">{value}</span>
      </div>
      <p className="text-xs text-muted-foreground">{label}</p>
    </div>
  );
}

/** The owed sentence is the status — never a PASS/FAIL stamp on today. */
function RuleStats({ rule }: { rule: RuleScore }) {
  return (
    <>
      <p className="mt-3 text-sm text-foreground">{rule.owed}</p>
      <p className="text-xs text-muted-foreground">
        {rule.actual_label} vs {rule.target_label}
      </p>
      <div className="mt-3 grid grid-cols-3 gap-2">
        <Stat
          icon={Flame}
          tone="text-gym-terracotta"
          value={String(rule.streak)}
          label="Streak"
        />
        <Stat
          icon={Target}
          tone="text-primary"
          value={
            rule.compliance === null
              ? '—'
              : `${Math.round(rule.compliance * 100)}%`
          }
          label="Compliance"
        />
        <Stat
          icon={Calendar}
          tone="text-work-blue"
          value={String(rule.best_streak)}
          label="Best"
        />
      </div>
    </>
  );
}

function RuleStrip({ rule }: { rule: RuleScore }) {
  return (
    // Fourteen equal columns: the cells are square and shrink with the card
    // instead of pushing a phone into a second row. `max-w-sm` keeps them the
    // size they were on a wide card.
    <div className="mt-3 grid max-w-sm grid-cols-14 gap-1">
      {rule.strip.map((cell) => (
        <div
          key={cell.date}
          role="img"
          aria-label={`${cell.date} ${cell.state}`}
          title={`${cell.date} ${cell.state}`}
          className={cn('aspect-square rounded', stripClass(cell.state))}
        />
      ))}
    </div>
  );
}

/** A card action: a plain button beside the header, never inside it. */
function CardAction({
  label,
  disabled,
  onClick,
  children,
}: {
  label: string;
  disabled?: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      title={label}
      aria-label={label}
      disabled={disabled}
      onClick={onClick}
      className="rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:opacity-40 disabled:hover:bg-transparent disabled:hover:text-muted-foreground"
    >
      {children}
    </button>
  );
}

/**
 * The card's shell: a border, the header (which is the toggle), the stats, and
 * the evidence region the header points at.
 *
 * Only the header toggles — the actions and the evidence sit outside it, so a
 * click on a chart or a button never collapses the card.
 */
function CardShell({
  id,
  expanded,
  onToggle,
  header,
  evidence,
  actions,
  className,
  children,
}: {
  id: string;
  expanded: boolean;
  onToggle: () => void;
  header: React.ReactNode;
  evidence: React.ReactNode;
  actions: React.ReactNode;
  className?: string;
  children: React.ReactNode;
}) {
  const evidenceId = `${id}-evidence`;
  return (
    <div
      className={cn(
        'rounded-xl border border-border bg-card p-4 shadow-sm transition-colors hover:border-primary/40',
        className,
      )}
    >
      <button
        type="button"
        aria-expanded={expanded}
        aria-controls={evidenceId}
        onClick={onToggle}
        className="w-full rounded-lg text-left focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/20"
      >
        {header}
      </button>
      {children}
      {actions}
      <div
        id={evidenceId}
        hidden={!expanded}
        className="mt-3 border-t border-border pt-3"
      >
        {evidence}
      </div>
    </div>
  );
}

// ──────────────────────────────────────────
// Board card
// ──────────────────────────────────────────

export function RuleCard({
  rule,
  index,
  total,
  expanded,
  onToggle,
  reorderEnabled,
  pending,
  onMove,
  onPause,
  onResume,
  onEdit,
  onArchive,
}: {
  rule: RuleScore;
  index: number;
  total: number;
  expanded: boolean;
  onToggle: () => void;
  reorderEnabled: boolean;
  pending: boolean;
  onMove: (direction: -1 | 1) => void;
  onPause: () => void;
  onResume: () => void;
  onEdit: () => void;
  onArchive: () => void;
}) {
  return (
    <CardShell
      id={`rule-${rule.id}`}
      expanded={expanded}
      onToggle={onToggle}
      header={
        <RuleHeader
          rule={rule}
          chip={<VerdictChip word={boardVerdict(rule)} />}
        />
      }
      evidence={<RuleEvidence rule={rule} />}
      actions={
        <div className="mt-3 flex items-center justify-end gap-1">
          <CardAction
            label="Move up"
            disabled={!reorderEnabled || index === 0}
            onClick={() => onMove(-1)}
          >
            <ChevronUp className="h-4 w-4" />
          </CardAction>
          <CardAction
            label="Move down"
            disabled={!reorderEnabled || index === total - 1}
            onClick={() => onMove(1)}
          >
            <ChevronDown className="h-4 w-4" />
          </CardAction>
          {rule.phase === 'paused' ? (
            <CardAction
              label={`Resume ${rule.name}`}
              disabled={pending}
              onClick={onResume}
            >
              <Play className="h-4 w-4" />
            </CardAction>
          ) : (
            <CardAction
              label={`Pause ${rule.name}`}
              disabled={pending}
              onClick={onPause}
            >
              <Pause className="h-4 w-4" />
            </CardAction>
          )}
          <CardAction
            label={`Edit ${rule.name}`}
            disabled={pending}
            onClick={onEdit}
          >
            <Pencil className="h-4 w-4" />
          </CardAction>
          <CardAction
            label={`Archive ${rule.name}`}
            disabled={pending}
            onClick={onArchive}
          >
            <Archive className="h-4 w-4" />
          </CardAction>
        </div>
      }
      className={rule.phase === 'paused' ? 'opacity-80' : undefined}
    >
      <RuleStats rule={rule} />
      <RuleStrip rule={rule} />
    </CardShell>
  );
}

// ──────────────────────────────────────────
// Archive row
// ──────────────────────────────────────────

export function ArchiveRuleRow({
  rule,
  expanded,
  onToggle,
  pending,
  onRestore,
  onReopen,
}: {
  rule: RuleScore;
  expanded: boolean;
  onToggle: () => void;
  pending: boolean;
  onRestore: () => void;
  onReopen: () => void;
}) {
  return (
    <CardShell
      id={`archive-${rule.id}`}
      expanded={expanded}
      onToggle={onToggle}
      header={
        <RuleHeader
          rule={rule}
          chip={
            <VerdictChip
              word={phaseLabel(rule.phase)}
              second={archiveVerdict(rule)}
            />
          }
        />
      }
      evidence={<RuleEvidence rule={rule} />}
      actions={
        <div className="mt-3 flex items-center justify-end gap-2">
          {rule.phase === 'finished' && (
            // A finished challenge is still `active` — reopening only clears
            // `ends_on`, which drops it back onto the board.
            <button
              type="button"
              disabled={pending}
              onClick={onReopen}
              className="inline-flex items-center gap-1 rounded-md border border-input px-2 py-1 text-xs text-foreground transition-colors hover:bg-muted disabled:opacity-50"
            >
              <RotateCcw className="h-3.5 w-3.5" />
              Reopen
            </button>
          )}
          <button
            type="button"
            disabled={pending}
            onClick={onRestore}
            className="inline-flex items-center gap-1 rounded-md border border-input px-2 py-1 text-xs text-foreground transition-colors hover:bg-muted disabled:opacity-50"
          >
            <Play className="h-3.5 w-3.5" />
            Restore
          </button>
        </div>
      }
    >
      <RuleStats rule={rule} />
      <RuleStrip rule={rule} />
    </CardShell>
  );
}
