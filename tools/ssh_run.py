"""Неинтерактивный запуск команд на сервере.

Пароль берётся из переменной окружения MB_PASS и никогда не попадает ни в код,
ни в аргументы команды (а значит и в историю оболочки, и в список процессов).

    $env:MB_HOST='<адрес>'; $env:MB_PORT='<порт>'
    $env:MB_USER='<логин>'; $env:MB_PASS='...'
    python tools/ssh_run.py "docker ps"

Адрес, порт и логин в код не зашиты: репозиторий может оказаться в открытом
доступе, а они вместе с паролем — это вход на сервер.

Канал до сервера рвётся регулярно, поэтому подключение делается с повторами.
Длинные операции (сборка образа) запускайте отсоединённо на той стороне:
    setsid nohup bash ~/deploy.sh > ~/deploy.log 2>&1 < /dev/null &
"""

import os
import sys
import time

import paramiko


def connect(attempts: int = 5) -> paramiko.SSHClient:
    host = os.environ.get("MB_HOST")
    port = int(os.environ.get("MB_PORT", "22"))
    user = os.environ.get("MB_USER")
    password = os.environ.get("MB_PASS")
    key_file = os.environ.get("MB_KEY")

    if not host or not user:
        sys.exit("не заданы MB_HOST и MB_USER")
    if not password and not key_file:
        sys.exit("не задан ни MB_PASS, ни MB_KEY")

    last = None
    for i in range(attempts):
        client = paramiko.SSHClient()
        client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
        try:
            client.connect(
                hostname=host,
                port=port,
                username=user,
                password=password or None,
                key_filename=key_file or None,
                timeout=25,
                banner_timeout=25,
                auth_timeout=25,
                look_for_keys=bool(key_file),
                allow_agent=False,
            )
            return client
        except Exception as e:  # обрыв канала — норма, пробуем ещё
            last = e
            client.close()
            if i < attempts - 1:
                time.sleep(4 * (i + 1))
    sys.exit(f"не удалось подключиться после {attempts} попыток: {last}")


def main() -> int:
    if len(sys.argv) < 2:
        sys.exit("использование: ssh_run.py \"<команда>\"")
    command = " ".join(sys.argv[1:])

    client = connect()
    try:
        _stdin, stdout, stderr = client.exec_command(command, timeout=180)
        out = stdout.read().decode("utf-8", "replace")
        err = stderr.read().decode("utf-8", "replace")
        code = stdout.channel.recv_exit_status()
    finally:
        client.close()

    if out:
        sys.stdout.write(out)
    if err:
        sys.stderr.write(err)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
