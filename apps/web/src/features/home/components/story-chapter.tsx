"use client";

import type { ReactNode } from "react";

import { useStoryFrames } from "../hooks/use-story-frames";

export interface DiagramProps {
  /** Current storyboard frame, `0..captions.length`. */
  readonly frame: number;
  /** Total frame advances; never wraps. */
  readonly tick: number;
}

/**
 * One storyboard chapter: copy and clickable frame captions beside an animated
 * diagram that steps through those frames while it is on screen.
 */
export function StoryChapter({
  id,
  number,
  kicker,
  title,
  children,
  captions,
  frameMs = 2600,
  diagramLabel,
  Diagram,
  flip = false,
  continuous = false,
}: {
  readonly id: string;
  readonly number: number;
  readonly kicker: string;
  readonly title: string;
  readonly children: ReactNode;
  readonly captions: readonly string[];
  readonly frameMs?: number;
  /** Text alternative describing the whole animation. */
  readonly diagramLabel: string;
  readonly Diagram: (props: DiagramProps) => ReactNode;
  readonly flip?: boolean;
  /** Steps at a constant pace, for diagrams that scroll rather than loop. */
  readonly continuous?: boolean;
}) {
  const { ref, frame, tick, playing, togglePlaying, goTo } = useStoryFrames(
    captions.length,
    frameMs,
    { holdLast: !continuous },
  );

  return (
    <section
      ref={ref}
      id={id}
      aria-labelledby={`${id}-title`}
      className={`mx-auto grid w-full max-w-6xl scroll-mt-20 items-center gap-8 px-4 py-16 sm:px-6 lg:gap-14 lg:py-24 ${
        flip
          ? "lg:grid-cols-[minmax(0,6fr)_minmax(0,5fr)]"
          : "lg:grid-cols-[minmax(0,5fr)_minmax(0,6fr)]"
      }`}
    >
      <div className={flip ? "lg:order-2" : undefined}>
        <div className="text-accent font-mono text-xs tracking-[0.2em] uppercase">
          {String(number).padStart(2, "0")} · {kicker}
        </div>
        <h2
          id={`${id}-title`}
          className="mt-3 text-3xl leading-tight font-semibold tracking-tight text-balance sm:text-4xl"
        >
          {title}
        </h2>
        <div className="text-muted mt-4 space-y-3 text-base leading-relaxed">
          {children}
        </div>
        <ol className="mt-7 space-y-1.5">
          {captions.map((caption, index) => {
            const active = index === frame;
            return (
              <li key={caption}>
                <button
                  type="button"
                  onClick={() => goTo(index)}
                  aria-current={active ? "step" : undefined}
                  className={`group flex w-full items-start gap-3 rounded-lg px-3 py-2 text-left text-sm transition-colors ${
                    active
                      ? "bg-raised text-text"
                      : "text-faint hover:text-muted"
                  }`}
                >
                  <span
                    className={`mt-0.5 font-mono text-xs ${
                      active ? "text-accent" : ""
                    }`}
                  >
                    {index + 1}
                  </span>
                  <span className="leading-snug">{caption}</span>
                </button>
              </li>
            );
          })}
        </ol>
      </div>

      <figure
        className={`border-line bg-panel relative overflow-hidden rounded-2xl border ${
          flip ? "lg:order-1" : ""
        }`}
      >
        <div
          role="img"
          aria-label={diagramLabel}
          className="px-3 pt-3 sm:px-5 sm:pt-5"
        >
          <Diagram frame={frame} tick={tick} />
        </div>
        <figcaption className="border-line flex items-center gap-3 border-t px-4 py-3">
          <button
            type="button"
            onClick={togglePlaying}
            aria-label={playing ? "Pause animation" : "Play animation"}
            className="text-muted hover:text-text grid size-8 shrink-0 place-items-center rounded-md"
          >
            {playing ? (
              <svg viewBox="0 0 16 16" className="size-3.5 fill-current">
                <rect x="3" y="2" width="3.5" height="12" rx="1" />
                <rect x="9.5" y="2" width="3.5" height="12" rx="1" />
              </svg>
            ) : (
              <svg viewBox="0 0 16 16" className="size-3.5 fill-current">
                <path d="M4 2.5v11a.5.5 0 0 0 .77.42l8.5-5.5a.5.5 0 0 0 0-.84l-8.5-5.5A.5.5 0 0 0 4 2.5Z" />
              </svg>
            )}
          </button>
          <p
            aria-live="polite"
            className="text-muted min-w-0 flex-1 text-sm leading-snug"
          >
            {captions[frame]}
          </p>
          <div className="flex shrink-0 gap-1.5" aria-hidden>
            {captions.map((caption, index) => (
              <span
                key={caption}
                className={`h-1.5 rounded-full transition-all duration-500 ${
                  index === frame ? "bg-accent w-5" : "bg-line-strong w-1.5"
                }`}
              />
            ))}
          </div>
        </figcaption>
      </figure>
    </section>
  );
}
