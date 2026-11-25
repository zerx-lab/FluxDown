import { expect, mock, test } from "bun:test";
import type { DispatchTarget } from "@/utils/download-dispatch";
import { createManualBatches } from "./manual-batches";
import type { SendResult } from "./protocol";

const remote = { remoteUrl: "https://named.example", remoteToken: "test-token", rejectRedirects: true };
const candidates = [{ candidateId: "video-a", variantId: "720p" }, { candidateId: "video-b" }];
const resourceIds = ["file-a", "file-b"];
const tabId = 42;
const referrer = "https://page.example/watch";
const links = Array.from({ length: 1001 }, (_, i) => `https://files.example/${i}.mp4`);
const sent: SendResult = { success: true, channel: "remote" };
const rejected: SendResult = { success: false, message: "server rejected", channel: "remote" };

type Target = DispatchTarget | undefined;

function setup(target: Target | null) {
  const chooseTarget = mock(async () => target);
  const sendCandidate = mock(async (_tab: number, _id: string, _variant: string | undefined, _target: Target) => sent);
  const sendResources = mock(async (_tab: number, _ids: string[], _target: Target) => sent);
  const sendUrlChunk = mock(async (_urls: string[], _referrer: string | undefined, _tab: number | undefined, _target: Target) => sent);
  return {
    actions: createManualBatches({ chooseTarget, sendCandidate, sendResources, sendUrlChunk }),
    chooseTarget, sendCandidate, sendResources, sendUrlChunk,
  };
}

test("a mixed resource selection keeps the chosen server and variants across every dispatch", async () => {
  const { actions, chooseTarget, sendCandidate, sendResources } = setup(remote);
  // A destination rejecting one item must not send that item or the rest to a different destination.
  sendCandidate.mockResolvedValueOnce(rejected);

  expect(await actions.downloadSelection(tabId, candidates, resourceIds)).toEqual(rejected);
  expect(chooseTarget).toHaveBeenCalledTimes(1);
  expect(sendCandidate.mock.calls).toEqual([
    [tabId, "video-a", "720p", remote],
    [tabId, "video-b", undefined, remote],
  ]);
  expect(sendResources.mock.calls).toEqual([[tabId, resourceIds, remote]]);
});

test("cancelling a mixed resource selection sends neither candidates nor files", async () => {
  const { actions, chooseTarget, sendCandidate, sendResources } = setup(null);

  expect(await actions.downloadSelection(tabId, candidates, resourceIds)).toEqual({ success: false, cancelled: true });
  expect(chooseTarget).toHaveBeenCalledTimes(1);
  expect(sendCandidate).not.toHaveBeenCalled();
  expect(sendResources).not.toHaveBeenCalled();
});

test("a link selection over the per-request cap keeps one destination and sends all links in order", async () => {
  const { actions, chooseTarget, sendUrlChunk } = setup(remote);

  expect(await actions.downloadManualUrls(links, referrer, tabId)).toEqual(sent);
  expect(chooseTarget).toHaveBeenCalledTimes(1);
  expect(sendUrlChunk.mock.calls).toEqual([
    [links.slice(0, 1000), referrer, tabId, remote],
    [links.slice(1000), referrer, tabId, remote],
  ]);
});

test("cancelling a multi-chunk link selection sends no chunks", async () => {
  const { actions, chooseTarget, sendUrlChunk } = setup(null);

  expect(await actions.downloadManualUrls(links, referrer, tabId)).toEqual({ success: false, cancelled: true });
  expect(chooseTarget).toHaveBeenCalledTimes(1);
  expect(sendUrlChunk).not.toHaveBeenCalled();
});

test("a rejected link chunk stops the batch without choosing another destination or reporting cancellation", async () => {
  const { actions, chooseTarget, sendUrlChunk } = setup(remote);
  sendUrlChunk.mockResolvedValueOnce(rejected);

  expect(await actions.downloadManualUrls(links, referrer, tabId)).toEqual(rejected);
  expect(chooseTarget).toHaveBeenCalledTimes(1);
  expect(sendUrlChunk.mock.calls).toEqual([[links.slice(0, 1000), referrer, tabId, remote]]);
});
