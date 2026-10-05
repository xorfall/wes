# Owned terminal bootstrap: no user startup files and no evaluation of prompt text.
shopt -u promptvars
unset HISTFILE
if [[ -n ${WES_HISTORY_FILE-} ]]; then
  readonly _wes_history_file=$WES_HISTORY_FILE
  HISTSIZE=1000
  unset HISTFILESIZE
  shopt -s cmdhist lithist
  _wes_history_entries=()
  _wes_history_last=0
  # Bash 3.2's history -r splits literal multiline entries. Private NUL-delimited
  # records preserve each command; history -s inserts text and never executes it.
  while IFS= read -r -d '' _wes_history_entry; do
    builtin history -s -- "$_wes_history_entry"
    _wes_history_entries+=("$_wes_history_entry")
    if (( ${#_wes_history_entries[@]} > 1000 )); then
      _wes_history_entries=("${_wes_history_entries[@]: -1000}")
    fi
  done < "$_wes_history_file"
  read -r _wes_history_last _ <<< "$(HISTTIMEFORMAT= builtin history 1)"
  _wes_history_saved=$_wes_history_last
  _wes_history_save() {
    local previous=$? number source listing previous_umask
    local header='^[[:blank:]]*([0-9]+)[ *] '
    # history -p deliberately excludes the currently executing entry in Bash 3.2.
    # history 1 includes it; remove only its numeric header and output newline.
    # A sentinel retains literal trailing newlines through command substitution.
    listing=$(HISTTIMEFORMAT= builtin history 1; printf '\001')
    listing=${listing%$'\001'}; listing=${listing%$'\n'}
    if [[ ! $listing =~ $header ]]; then return "$previous"; fi
    number=${BASH_REMATCH[1]}
    if [[ $number == "$_wes_history_saved" ]]; then return "$previous"; fi
    source=${listing#"${BASH_REMATCH[0]}"}
    if [[ $number != "$_wes_history_last" ]]; then
      _wes_history_last=$number
      _wes_history_entries+=("$source")
      if (( ${#_wes_history_entries[@]} > 1000 )); then
        _wes_history_entries=("${_wes_history_entries[@]: -1000}")
      fi
    fi
    previous_umask=$(umask)
    umask 077
    if printf '%s\0' "${_wes_history_entries[@]}" > "${_wes_history_file}.next" &&
      /bin/mv -f "${_wes_history_file}.next" "$_wes_history_file"; then
      _wes_history_saved=$number
    fi
    umask "$previous_umask"
    return "$previous"
  }
fi
unset WES_HISTORY_FILE
readonly _wes_git=${WES_PROMPT_GIT-}
unset WES_PROMPT_GIT
readonly _wes_environment_file=${WES_PROMPT_ENVIRONMENT-}
unset WES_PROMPT_ENVIRONMENT

_wes_prompt() {
  local previous=$? branch='' label directory environment='no-env'
  # Begin recording only after startup; HISTFILE stays unset so Bash cannot
  # overwrite our atomic snapshot with its own non-atomic shutdown write.
  if [[ -n ${_wes_history_file-} && ${_wes_history_ready-} != 1 ]]; then
    _wes_history_ready=1
    trap '_wes_history_save' DEBUG
  fi
  if [[ -n $_wes_git ]]; then
    if ! branch=$(GIT_OPTIONAL_LOCKS=0 "$_wes_git" --no-pager -c core.fsmonitor=false symbolic-ref --quiet --short HEAD 2>/dev/null); then
      branch=$(GIT_OPTIONAL_LOCKS=0 "$_wes_git" --no-pager -c core.fsmonitor=false rev-parse --short HEAD 2>/dev/null) &&
        branch="@${branch}" || branch=''
    fi
  fi
  if [[ -n $_wes_environment_file && -r $_wes_environment_file ]]; then
    IFS= read -r environment < "$_wes_environment_file" || environment='no-env'
  fi
  label="${USER:-user}@${environment:-no-env}"
  directory=$PWD
  if [[ $PWD == "$HOME" ]]; then
    directory='~'
  elif [[ $PWD == "$HOME/"* ]]; then
    directory="~/${PWD#"$HOME/"}"
  fi
  # With promptvars off, only backslash escapes remain special to bash's prompt decoder.
  label=${label//[[:cntrl:]]/?}; label=${label//\\/\\\\}
  directory=${directory//[[:cntrl:]]/?}; directory=${directory//\\/\\\\}
  branch=${branch//[[:cntrl:]]/?}; branch=${branch//\\/\\\\}
  PS1='\[\e[0m\e[32m\]'"${label}"'\[\e[0m\] \[\e[36m\]'"${directory}"'\[\e[0m\]'
  [[ -n $branch ]] && PS1+=' (\[\e[33m\]'"${branch}"'\[\e[0m\])'
  PS1+='\[\e[0m\] % '
  return "$previous"
}
PROMPT_COMMAND=_wes_prompt
