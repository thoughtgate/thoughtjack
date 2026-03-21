import React from 'react';

interface TutorialProgressProps {
  step: number;
  total: number;
}

/**
 * Displays a visual progress indicator at the top of tutorial pages.
 *
 * Usage in MDX:
 * ```mdx
 * import {TutorialProgress} from '@site/src/components/TutorialProgress';
 *
 * <TutorialProgress step={1} total={3} />
 * ```
 */
export function TutorialProgress({step, total}: TutorialProgressProps): React.ReactElement {
  return (
    <div className="tutorial-progress">
      <span className="tutorial-progress__label">
        Tutorial {step} of {total}
      </span>
      <div className="tutorial-progress__bar">
        {Array.from({length: total}, (_, i) => (
          <div
            key={i}
            className={[
              'tutorial-progress__dot',
              i < step ? 'tutorial-progress__dot--complete' : '',
              i === step - 1 ? 'tutorial-progress__dot--current' : '',
            ]
              .filter(Boolean)
              .join(' ')}
          />
        ))}
      </div>
    </div>
  );
}
