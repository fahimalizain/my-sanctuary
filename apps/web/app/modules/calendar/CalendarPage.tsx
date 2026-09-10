import { useLayoutEffect } from 'react';
import { CalendarHeader } from './components/CalendarHeader';
import { CalendarSidebar } from './components/CalendarSidebar';
import { CalendarSyncBanner } from './components/CalendarSyncBanner';
import { CalendarWeekGrid } from './components/CalendarWeekGrid';
import { EventInspector } from './components/EventInspector';
import { useCalendarSession } from './hooks/useCalendarSession';
import { useCalendarStrip } from './hooks/useCalendarStrip';
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
    setStripLocked: strip.setStripLocked,
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
          <p className="text-muted-foreground truncate">{session.writeError}</p>
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
          onScroll={strip.onScrollerScroll}
          contentWidth={strip.contentWidth}
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
          allDayHeight={session.allDayHeight}
          allDayChips={session.allDayChips}
          todayIndex={session.todayIndex}
          eventsByDay={session.eventsByDay}
          selectedEventId={session.selectedEventId}
          draggingEventId={session.draggingEventId}
          onColumnPointerDown={session.onColumnPointerDown}
          onChipPointerDown={session.onChipPointerDown}
          onSelectChip={session.handleSelectChip}
          onAllDayPointerDown={session.onAllDayPointerDown}
          onAllDayChipPointerDown={session.onAllDayChipPointerDown}
          onChipSelect={session.handleSelectChip}
          isDragging={session.isDragging}
          timedPreviewByDay={session.timedPreviewByDay}
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
            focusTitle={session.focusTitleOnOpen}
            onClose={session.closeInspector}
            onSaveTitle={session.handleSaveTitle}
            onSaveDescription={session.handleSaveDescription}
            onDelete={session.handleDeleteEvent}
            isSaving={session.isSaving}
            isDeleting={session.isDeleting}
            isDragging={session.isDragging}
          />
        )}
      </div>
    </div>
  );
}
