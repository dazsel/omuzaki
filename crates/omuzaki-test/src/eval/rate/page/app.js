// The rating page. Everything shown comes from this machine's server; ratings go back to it and nowhere else.
// Text is always set with textContent, never as markup: a prompt or a question is data.
'use strict';

const state = { token: '', items: [], at: 0, answers: new Map(), finished: false };

const $ = (id) => document.getElementById(id);
const key = (item, question) => item.id + '\u0000' + question;

function say(text, isError) {
  const progress = $('progress');
  progress.textContent = text;
  progress.className = isError ? 'error' : '';
}

function current() {
  return state.items[state.at];
}

function firstOpen(item) {
  return item.questions.find((q) => !state.answers.has(key(item, q)));
}

function render() {
  const item = current();
  if (!item) {
    say('Nothing to rate: every result is already rated, or this week’s limit is reached.');
    $('questions').textContent = '';
    $('pictures').textContent = '';
    $('prompt').textContent = '';
    for (const id of ['previous', 'next']) $(id).disabled = true;
    return;
  }
  const answered = state.items.filter((i) => !firstOpen(i)).length;
  say('Item ' + (state.at + 1) + ' of ' + state.items.length + ' · ' + answered + ' finished');
  $('prompt').textContent = item.prompt;

  const pictures = $('pictures');
  pictures.textContent = '';
  state.pictureNames.forEach((name, n) => {
    const figure = document.createElement('figure');
    if (item.pictures[n]) {
      const image = document.createElement('img');
      image.src = '/img/' + state.at + '/' + name + '.png';
      image.alt = 'The ' + name + ' render';
      figure.append(image);
    } else {
      const none = document.createElement('div');
      none.className = 'none';
      none.textContent = 'No ' + name + ' render kept for this result';
      figure.append(none);
    }
    const caption = document.createElement('figcaption');
    caption.textContent = name[0].toUpperCase() + name.slice(1);
    figure.append(caption);
    pictures.append(figure);
  });

  const box = $('questions');
  box.textContent = '';
  for (const question of item.questions) {
    const row = document.createElement('div');
    row.className = 'question';
    const text = document.createElement('p');
    text.textContent = question;
    text.id = 'q' + item.questions.indexOf(question);
    const choices = document.createElement('div');
    choices.className = 'choices';
    choices.setAttribute('role', 'group');
    choices.setAttribute('aria-labelledby', text.id);
    const given = state.answers.get(key(item, question));
    for (const answer of ['yes', 'no', 'unsure']) {
      const button = document.createElement('button');
      button.type = 'button';
      button.dataset.answer = answer;
      button.textContent = answer[0].toUpperCase() + answer.slice(1);
      button.setAttribute('aria-pressed', String(given === answer));
      button.disabled = given !== undefined;
      button.addEventListener('click', () => rate(item, question, answer));
      choices.append(button);
    }
    row.append(text, choices);
    box.append(row);
  }
  $('previous').disabled = state.at === 0;
  $('next').disabled = state.at >= state.items.length - 1;
}

async function rate(item, question, answer) {
  if (state.answers.has(key(item, question))) return;
  const response = await fetch('/api/rate', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Rating-Token': state.token },
    body: JSON.stringify({ item: item.id, question, answer }),
  });
  if (!response.ok) {
    say('The rating was not saved: ' + (await response.text()), true);
    return;
  }
  const result = await response.json();
  state.answers.set(key(item, question), answer);
  if (result.done) {
    state.finished = true;
    render();
    say('All done: ' + state.items.length + ' results rated. You can close this page.');
    for (const id of ['previous', 'next', 'finish']) $(id).disabled = true;
    return;
  }
  if (!firstOpen(item) && state.at < state.items.length - 1) state.at += 1;
  render();
  focusNext();
}

function focusNext() {
  const item = current();
  const question = item && firstOpen(item);
  const index = question === undefined ? -1 : item.questions.indexOf(question);
  const target = index >= 0 ? document.querySelectorAll('.question')[index].querySelector('button') : $('next');
  if (target && !target.disabled) target.focus();
}

function move(by) {
  const next = state.at + by;
  if (next < 0 || next >= state.items.length) return;
  state.at = next;
  render();
  focusNext();
}

async function finish() {
  await fetch('/api/quit', { method: 'POST', headers: { 'X-Rating-Token': state.token } });
  state.finished = true;
  say('Finished. You can close this page.');
  for (const id of ['previous', 'next', 'finish']) $(id).disabled = true;
  for (const button of document.querySelectorAll('.choices button')) button.disabled = true;
}

document.addEventListener('keydown', (event) => {
  if (state.finished || event.ctrlKey || event.metaKey || event.altKey) return;
  const item = current();
  if (!item) return;
  const open = firstOpen(item);
  const answers = { y: 'yes', n: 'no', u: 'unsure' };
  if (answers[event.key.toLowerCase()] && open !== undefined) {
    event.preventDefault();
    rate(item, open, answers[event.key.toLowerCase()]);
  } else if (event.key === 'ArrowRight') {
    event.preventDefault();
    move(1);
  } else if (event.key === 'ArrowLeft') {
    event.preventDefault();
    move(-1);
  }
});

$('previous').addEventListener('click', () => move(-1));
$('next').addEventListener('click', () => move(1));
$('finish').addEventListener('click', finish);

fetch('/api/items')
  .then((response) => response.json())
  .then((data) => {
    state.token = data.token;
    state.items = data.items;
    state.pictureNames = data.pictures;
    render();
    $('main').focus();
    focusNext();
  })
  .catch(() => say('The page could not load its results from this machine’s server.', true));
