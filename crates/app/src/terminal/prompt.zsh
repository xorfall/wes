# Owned terminal bootstrap: never source user startup files or enable prompt substitution.
unsetopt PROMPT_SUBST
setopt PROMPT_PERCENT
PROMPT_EOL_MARK=''
unset HISTFILE
if [[ -n ${WES_HISTORY_FILE-} ]]; then
  readonly _wes_history_file=$WES_HISTORY_FILE
  _wes_history_bytes=${WES_HISTORY_BYTES:-0}
  case $_wes_history_bytes in *[!0-9]*|'') _wes_history_bytes=0 ;; esac
  readonly _wes_history_bytes=$((10#$_wes_history_bytes))
  HISTSIZE=1000
  _wes_history_entries=()
  _wes_history_size=0
  _wes_history_count_bytes() {
    local LC_ALL=C
    _wes_history_octets=${#1}
  }
  _wes_history_add() {
    _wes_history_entries+=("$1")
    _wes_history_count_bytes "$1"
    ((_wes_history_size += _wes_history_octets + 1))
    while (( ${#_wes_history_entries[@]} > 1000 ||
             (_wes_history_bytes > 0 && _wes_history_size > _wes_history_bytes && ${#_wes_history_entries[@]} > 0) )); do
      _wes_history_count_bytes "${_wes_history_entries[1]}"
      ((_wes_history_size -= _wes_history_octets + 1))
      shift _wes_history_entries
    done
  }
  # Restore literal commands into zsh's history list, never shell source/eval.
  while IFS= read -r -d '' _wes_history_entry; do
    print -sr -- "$_wes_history_entry"
    _wes_history_add "$_wes_history_entry"
  done < "$_wes_history_file"
  _wes_history_save() {
    local previous=$? previous_umask
    # preexec receives the full accepted source, including literal newlines, before
    # a long-running command begins; each multiline source remains one record.
    _wes_history_add "$1"
    previous_umask=$(umask)
    umask 077
    # No retained commands is an empty file; printf with no arguments would emit a lone NUL.
    { if (( ${#_wes_history_entries[@]} )); then
        printf '%s\0' "${_wes_history_entries[@]}"
      fi; } > "${_wes_history_file}.next" &&
      /bin/mv -f "${_wes_history_file}.next" "$_wes_history_file"
    umask "$previous_umask"
    return "$previous"
  }
  preexec_functions+=(_wes_history_save)
fi
unset WES_HISTORY_FILE WES_HISTORY_BYTES
readonly _wes_git=${WES_PROMPT_GIT-}
unset WES_PROMPT_GIT
readonly _wes_environment_file=${WES_PROMPT_ENVIRONMENT-}
unset WES_PROMPT_ENVIRONMENT
# Nested shells explicitly started by the user use their ordinary startup location.
unset ZDOTDIR

_wes_prompt() {
  local previous=$? branch='' label directory environment='no-env'
  # These builtins only read refs; they do not scan files, invoke hooks or contact remotes.
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
  # Strip terminal controls, then quote percent expansion. No external text is evaluated.
  label=${label//[[:cntrl:]]/?}; label=${label//\%/%%}
  directory=${directory//[[:cntrl:]]/?}; directory=${directory//\%/%%}
  branch=${branch//[[:cntrl:]]/?}; branch=${branch//\%/%%}
  PROMPT=$'%{\e[0m%}'"%F{green}${label}%f %F{cyan}${directory}%f"
  [[ -n $branch ]] && PROMPT+=" (%F{yellow}${branch}%f)"
  PROMPT+=$'%{\e[0m%} %% '
  return $previous
}
precmd_functions+=(_wes_prompt)
