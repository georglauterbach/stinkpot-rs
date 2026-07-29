#! /usr/bin/env bash

# run eval "$(stinkpot init)" at startup
__stinkpot_record() {
  local CMD EXIT=${?}
  CMD=$(HISTTIMEFORMAT='' history 1 | sed '1 s/^[[:space:]]*[0-9]\{1,\}[[:space:]]*//')
  [[ -n ${CMD} ]] && stinkpot add --exit "${EXIT}" -- "${CMD}"
}

__stinkpot_search() {
  local OUT
  OUT=$(stinkpot search -- "${READLINE_LINE}") || return
  if [[ -n ${OUT} ]]; then
    READLINE_LINE=${OUT}
    READLINE_POINT=${#READLINE_LINE}
  fi
}

if [[ ${PROMPT_COMMAND} != *__stinkpot_record* ]]; then
  PROMPT_COMMAND="__stinkpot_record${PROMPT_COMMAND+; ${PROMPT_COMMAND}}"
fi

bind -x '"\C-r": __stinkpot_search'
