import { useCallback, useLayoutEffect, useState } from 'react';
import { CalendarHeader } from './components/CalendarHeader';
import { CalendarSidebar } from './components/CalendarSidebar';
import { CalendarSyncBanner } from './components/CalendarSyncBanner';
import { CalendarWeekGrid } from './components/CalendarWeekGrid';
import { DeleteEventDialog } from './components/DeleteEventDialog';
import {
  EventInspector,
  isCalendarWritable,
} from './components/EventInspector';
import { useCalendarSession } from './hooks/useCalendarSession';
import { useCalendarShortcuts } from './hooks/useCalendarShortcuts';
import { useCalendarStrip } from './hooks/useCalendarStrip';
import { useHideNavOnLateHours } from './hooks/useHideNavOnLateHours';
import {
  clipboardItemFromEvent,
  type CalendarClipboardItem,
} from './lib/calendar-clipboard';
import { keyboardCreateRange } from './lib/calendar-model';
import { COL_HEADER_H, addDays, isSameDay, nowLineY } from './lib/week-layout';

export function CalendarPage() {
  const strip = useCalendarStrip();
  const session = useCalendarSession({
    timeMin: strip.range.timeMin,
    timeMax: strip.range.timeMax,
    days: strip.days,
    dayCount: strip.dayCount,
    scrollerHeight: strip.scrollerHeight,
    scrollerRef: strip.scrollerRef,
    lockStrip: strip.lockStrip,
    unlockStrip: strip.unlockStrip,
    scrollStripBy: strip.scrollStripBy,
  });

  useHideNavOnLateHours(strip.scrollerRef, session.hourH, session.allDayHeight);

  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  // In-memory clipboard for Cmd/Ctrl+C / X / V. Page state on purpose: this is
  // not the system clipboard, so copying an event never touches the OS.
  const [clipboard, setClipboard] = useState<CalendarClipboardItem | null>(
    null,
  );
  const selectedId = session.selectedEvent?.id ?? null;
  // The dialog belongs to the selection it was opened for.
  const confirmOpen =
    confirmDeleteId !== null && confirmDeleteId === selectedId;

  const canTouchDelete =
    session.selectedEvent !== null &&
    isCalendarWritable(session.selectedEventCalendar);

  // Stable across drag/now re-renders so memoized DayColumns keep their props;
  // it only changes with the selection.
  const requestDelete = useCallback(
    () => setConfirmDeleteId(selectedId),
    [selectedId],
  );

  // One page-level keymap: the strip no longer listens on its own. The delete
  // dialog and an active pointer drag suspend it (their focused buttons are not
  // editing targets, so the target check alone would let keys through); Escape
  // during a drag stays with the capture-phase handler inside useCalendarDrag.
  useCalendarShortcuts({
    suspended: confirmOpen || session.isDragging,
    onPeriodChange: strip.handlePeriodChange,
    onToday: strip.goToToday,
    onShiftPeriod: strip.shiftPeriod,
    onCreate: () => {
      session.beginDraft(
        keyboardCreateRange({
          now: session.now,
          visibleStart: strip.visibleStart,
          periodLength: strip.periodLength,
        }),
      );
    },
    onRequestDelete: canTouchDelete ? requestDelete : null,
    // Copy and cut fill the page clipboard; paste consumes the key only when
    // there was something to paste. Duplicate never writes the clipboard.
    onCopy: () => {
      const item = clipboardItemFromEvent(session.selectedEvent);
      if (!item) return false;
      setClipboard(item);
      return true;
    },
    onCut: () => {
      const item = session.cutForClipboard();
      if (!item) return false;
      setClipboard(item);
      return true;
    },
    onPaste: () => {
      if (!clipboard) return false;
      const item = clipboard;
      // True only when the cut source was actually deleted; an abandoned or
      // ordinary paste leaves the clipboard item as-is.
      void session.pasteFromClipboard(item).then((removedCutSource) => {
        if (!removedCutSource) return;
        // Only this cut's own item is cleared: a copy that replaced the
        // clipboard while the paste was in flight must keep its cutSourceId.
        setClipboard((prev) =>
          prev && prev.cutSourceId === item.cutSourceId
            ? { ...prev, cutSourceId: null }
            : prev,
        );
      });
      return true;
    },
    onDuplicate: () => session.duplicateSelected(),
  });

  // Scroll so the now line sits ~⅓ down the visible hours area — once per
  // mount / Today jump, and only when the visible period contains today.
  useLayoutEffect(() => {
    if (!strip.shouldScrollToNowRef.current) return;
    if (session.availableHoursPx <= 0 || session.hourH <= 0) return;

    const scroller = strip.scrollerRef.current;
    if (!scroller) return;

    const visibleDays = Array.from({ length: strip.periodLength }, (_, i) =>
      addDays(strip.visibleStart, i),
    );
    const periodContainsToday = visibleDays.some((d) =>
      isSameDay(d, strip.today),
    );
    if (!periodContainsToday) {
      strip.shouldScrollToNowRef.current = false;
      return;
    }

    const y = nowLineY(session.now, session.hourH);
    // Hours sit below sticky headers + all-day inside the content box.
    const contentY = COL_HEADER_H + session.allDayHeight + y;
    const visibleH = Math.max(0, scroller.clientHeight);
    scroller.scrollTop = Math.max(0, contentY - visibleH / 3);
    strip.shouldScrollToNowRef.current = false;
  }, [
    session.availableHoursPx,
    session.hourH,
    strip.visibleStart,
    strip.periodLength,
    strip.today,
    session.now,
    session.allDayHeight,
    strip.shouldScrollToNowRef,
    strip.scrollerRef,
  ]);

  // Long-press selection is disabled app-wide in globals.css; form fields opt
  // back in there, and `.selectable` covers any static text worth copying.
  return (
    <div className="h-[100dvh] bg-cream flex flex-col">
      <CalendarHeader
        rangeTitle={strip.rangeTitle}
        isRefreshing={session.isRefreshing}
        periodLength={strip.periodLength}
        onPeriodChange={strip.handlePeriodChange}
        onToday={strip.goToToday}
        onPrevPeriod={() => strip.shiftPeriod(-1)}
        onNextPeriod={() => strip.shiftPeriod(1)}
      />

      <CalendarSyncBanner
        banner={session.healthBanner}
        onRetry={session.retry}
        isRefreshing={session.isRefreshing}
      />

      {session.writeError && (
        <div className="shrink-0 flex items-center justify-between gap-3 border-b border-border px-4 py-1.5 text-xs">
          <p className="selectable text-muted-foreground truncate">
            {session.writeError}
          </p>
          <button
            type="button"
            onClick={session.clearWriteError}
            className="shrink-0 text-muted-foreground hover:text-foreground underline-offset-2 hover:underline"
          >
            Dismiss
          </button>
        </div>
      )}

      {/* Body: sidebar + grid + inspector */}
      <div className="flex-1 min-h-0 flex relative">
        <CalendarSidebar
          weekStart={strip.visibleStart}
          periodLength={strip.periodLength}
          onGoToDate={strip.goToDate}
          calendars={session.calendars}
          calendarsLoading={session.calendarsLoading}
          calendarsError={session.calendarsError}
          onRetryCalendars={session.retryCalendars}
          selectedCalendarIds={session.selectedCalendarIds}
          onToggleCalendar={session.toggleCalendar}
        />

        <CalendarWeekGrid
          gridColumnRef={strip.gridColumnRef}
          scrollerRef={strip.scrollerRef}
          colW={strip.colW}
          gutterW={strip.gutterW}
          trackWidth={strip.trackWidth}
          days={strip.days}
          today={strip.today}
          now={session.now}
          hourH={session.hourH}
          totalHoursH={session.totalHoursH}
          hourGridBg={session.hourGridBg}
          hourLabels={session.hourLabels}
          gutterEdges={session.gutterEdges}
          allDayHeight={session.allDayHeight}
          allDayChips={session.allDayChips}
          todayIndex={session.todayIndex}
          eventsByDay={session.eventsByDay}
          selectedEventId={session.selectedEventId}
          draggingEventId={session.draggingEventId}
          onColumnPointerDown={session.onColumnPointerDown}
          onChipPointerDown={session.onChipPointerDown}
          onSelectChip={session.handleSelectChip}
          onRequestDelete={canTouchDelete ? requestDelete : undefined}
          onAllDayPointerDown={session.onAllDayPointerDown}
          onAllDayChipPointerDown={session.onAllDayChipPointerDown}
          onGutterEdgePointerDown={session.onGutterEdgePointerDown}
          onChipSelect={session.handleSelectChip}
          isDragging={session.isDragging}
          precisionSnap={session.precisionSnap}
          timedPreviewByDay={session.timedPreviewByDay}
          previewInfo={session.previewInfo}
          previewColor={session.previewColor}
          previewTitle={session.previewTitle}
          allDayPreview={session.allDayPreview}
          isLoading={session.isLoading}
          eventsEmpty={session.eventsEmpty}
          error={session.error}
          onRetry={session.retry}
        />

        {session.selectedEvent && (
          <EventInspector
            event={session.selectedEvent}
            calendar={session.selectedEventCalendar}
            calendars={session.calendars}
            focusTitle={session.focusTitleOnOpen}
            onClose={session.closeInspector}
            onSaveTitle={session.handleSaveTitle}
            onSaveDescription={session.handleSaveDescription}
            onSaveTimes={session.handleSaveTimes}
            onSaveAllDay={session.handleSaveAllDay}
            onSaveTimeZone={session.handleSaveTimeZone}
            onSaveCalendar={session.handleSaveCalendar}
            onDelete={session.handleDeleteEvent}
            isDeleting={session.isDeleting}
            isDragging={session.isDragging}
          />
        )}
      </div>

      <DeleteEventDialog
        open={confirmOpen}
        title={session.selectedEvent?.title ?? ''}
        isDeleting={session.isDeleting}
        onCancel={() => setConfirmDeleteId(null)}
        onConfirm={() => {
          setConfirmDeleteId(null);
          void session.handleDeleteEvent();
        }}
      />
    </div>
  );
}
