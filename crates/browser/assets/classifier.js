// MIT, codex-chatgpt-web contributors. Extracted classifier; TypeScript annotations removed.

      // Self-contained so the test suite can execute this exact source against a synthetic DOM;
      // it must not close over anything from the surrounding evaluate scope.
      const selectChatGptAnswerRoots = (
        markdownRoots,
        statusContainers,
        activityContainers = [],
      ) => {
        const firstStatusContainer = statusContainers[0];
        const commentary = markdownRoots.filter(candidate => (
          (!candidate.closest("[data-content-search-unit-key]")
            && activityContainers.some(container => container.contains(candidate)))
          || candidate.closest("[data-streaming-response-status]") !== null
          // Chain-of-thought components carry reasoning, never the final answer, so containment is
          // a position-independent commentary signal. Position alone cannot separate "commentary
          // between two status containers" from "answer between two tool calls".
          || candidate.closest('[data-testid^="cot-v5"]') !== null
          // Only Markdown that precedes the FIRST status container is prior commentary. Keying
          // this on "some status follows me" silently reclassified answer text as commentary as
          // soon as a second tool call opened another status container below it, which both zeroed
          // the visible text and dropped every answer chunk emitted between tool calls.
          || (firstStatusContainer !== undefined && Boolean(
            // 4 is Node.DOCUMENT_POSITION_FOLLOWING, inlined to keep this function standalone.
            candidate.compareDocumentPosition(firstStatusContainer) & 4,
          ))
        ));
        return {
          commentaryRoots: commentary,
          answerRoots: markdownRoots.filter(candidate => !commentary.includes(candidate)),
        };
      };
      