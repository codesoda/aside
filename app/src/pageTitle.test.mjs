import assert from 'node:assert/strict';
import { test } from 'node:test';
import { reviewPageTitle } from './pageTitle.ts';

const pr = { owner: 'codesoda', repo: 'aside', number: 88, title: 'Improve review navigation' };

test('page titles identify the review, PR number and repository', () => {
  assert.equal(reviewPageTitle(pr), '#88: Improve review navigation · codesoda/aside · Aside');
  assert.equal(reviewPageTitle({ ...pr, number: 88n }), reviewPageTitle(pr));
  assert.notEqual(reviewPageTitle({ ...pr, number: 89 }), reviewPageTitle(pr));
  assert.notEqual(reviewPageTitle({ ...pr, repo: 'another-repo' }), reviewPageTitle(pr));
});

test('page titles keep the available identity when metadata is missing', () => {
  for (const [metadata, expected] of [
    [{ ...pr, number: undefined }, 'Improve review navigation · codesoda/aside · Aside'],
    [{ ...pr, number: null }, 'Improve review navigation · codesoda/aside · Aside'],
    [{ ...pr, title: undefined }, '#88 · codesoda/aside · Aside'],
    [{ ...pr, title: ' \t ' }, '#88 · codesoda/aside · Aside'],
    [{ owner: 'codesoda', repo: 'aside' }, 'Review · codesoda/aside · Aside'],
    [{ repo: 'aside' }, 'Review · aside · Aside'],
    [{ title: 'Local changes' }, 'Local changes · Aside'],
    [{ number: 0 }, 'Review · Aside'],
    [{}, 'Review · Aside'],
    [null, 'Review · Aside'],
    [undefined, 'Review · Aside'],
  ]) {
    assert.equal(reviewPageTitle(metadata), expected);
  }
});

test('page titles trim empty edges without truncating or encoding review text', () => {
  const title = '修正 👩🏽‍💻 café & "quotes" <tags> '.repeat(40).trim();
  assert.equal(reviewPageTitle({ ...pr, title: ` ${title} `, owner: ' codesoda ', repo: ' aside ' }),
    `#88: ${title} · codesoda/aside · Aside`);
});
