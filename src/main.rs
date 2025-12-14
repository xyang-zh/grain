use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    execute,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::Rect,
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame, Terminal,
    style::{Color, Style}
};
use clap::{Arg, Command};
use std::io::{self, BufRead, BufReader};
use std::panic;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};
use std::fs::File;

#[derive(Debug)]
struct AppConfig {
    interval: Duration,
    file: Option<String>,
    command: Option<(String, Vec<String>)>,
}

struct App {
    config: AppConfig,
    state: DisplayState,
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

fn parse_args() -> AppConfig {
    let matches = Command::new("grain")
        .version("1.0")
        .arg(
            Arg::new("interval")
                .short('i')
                .long("interval")
                .value_name("INTERVAL")
                .help("100ms, 1, 2s (100ms起, 默认1秒)")
                .default_value("1s")
        )
        .arg(
            Arg::new("file")
                .short('f')
                .long("file")
                .value_name("FILE")
                .help("文件 (默认: /proc/interrupts)")
        )
        .arg(
            Arg::new("command")
                .short('c')
                .long("command")
                .value_name("COMMAND")
                .num_args(1..)
                .value_delimiter(' ')
                .help("命令")
        )
        .arg(
            Arg::new("speed")
                .short('s')
                .long("speed")
                .value_name("SPEED")
                .help("调整刷新速度倍率 (0.1-10.0)")
        )
        .after_help(
            "\n用法:\n  \
              ↑/↓          垂直滚动\n  \
              ←/→          水平滚动\n  \
              PgUp/PgDn    垂直翻页\n  \
              Home/End     水平跳转\n  \
              Ctrl+Home/End   垂直跳转\n  \
              q/Ctrl+C     退出"
        )
        .get_matches();

    let interval_str = matches.get_one::<String>("interval").unwrap();
    let base_interval = parse_interval(interval_str).unwrap_or_else(|e| {
        eprintln!("错误: {}", e);
        std::process::exit(1);
    });

    let interval = if let Some(speed_str) = matches.get_one::<String>("speed") {
        let speed = speed_str.parse::<f64>().unwrap_or(1.0).clamp(0.1, 10.0);
        Duration::from_millis((base_interval.as_millis() as f64 / speed) as u64)
    } else {
        base_interval
    };

    AppConfig {
        interval,
        file: matches.get_one::<String>("file").map(|s| s.to_string()),
        command: if let Some(cmd_parts) = matches.get_many::<String>("command") {
            let parts: Vec<String> = cmd_parts.map(|s| s.to_string()).collect();
            if !parts.is_empty() {
                Some((parts[0].clone(), parts[1..].to_vec()))
            } else {
                None
            }
        } else {
            None
        },
    }
}

fn parse_interval(interval_str: &str) -> Result<Duration, String> {
    let interval_str = interval_str.trim().to_lowercase();
    
    let (value_str, unit) = if interval_str.ends_with("ms") {
        (&interval_str[..interval_str.len() - 2], "ms")
    } else if interval_str.ends_with('s') {
        (&interval_str[..interval_str.len() - 1], "s")
    } else {
        (&interval_str[..], "s")
    };
    
    let value = value_str.parse::<f64>().map_err(|e| format!("无效的时间值: {}", e))?;
    
    let ms = match unit {
        "ms" => value as u64,
        "s" => (value * 1000.0) as u64,
        _ => return Err("不支持的时间单位".to_string()),
    };
    
    if ms < 100 {
        return Err("间隔不能小于100毫秒".to_string());
    }
    
    Ok(Duration::from_millis(ms))
}

fn visual_width(line: &str) -> usize {
    let mut in_escape = false;
    let mut width = 0;
    
    for c in line.chars() {
        if c == '\x1b' {
            in_escape = true;
            continue;
        }
        if in_escape {
            if c == 'm' {
                in_escape = false;
            }
            continue;
        }
        
        width += 1;
    }
    
    width
}

fn crop_line_for_scroll(line: &str, scroll_x: u16) -> String {
    if scroll_x == 0 {
        return line.to_string();
    }

    let scroll_x_usize = scroll_x as usize;
    let mut result = String::new();
    let mut in_escape = false;
    let mut escape_buffer = String::new();
    let mut visual_pos = 0;
    
    for c in line.chars() {
        if in_escape {
            escape_buffer.push(c);
            if c == 'm' {
                in_escape = false;
                result.push_str(&escape_buffer);
                escape_buffer.clear();
            }
        } else if c == '\x1b' {
            in_escape = true;
            escape_buffer.push(c);
        } else {
            if visual_pos >= scroll_x_usize {
                result.push(c);
            }
            visual_pos += 1;
        }
    }
    
    if in_escape {
        result.push_str(&escape_buffer);
    }
    
    if result.is_empty() && !line.is_empty() {
        return " ".to_string();
    }

    result
}

fn read_content(config: &AppConfig) -> io::Result<Vec<String>> {
    if let Some((cmd, args)) = &config.command {
        let mut child = ProcessCommand::new(cmd)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        
        let timeout = config.interval.mul_f64(0.8)
            .max(Duration::from_millis(100))
            .min(Duration::from_secs(3));
        
        let start_time = Instant::now();
        
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    break;
                }
                Ok(None) => {
                    if start_time.elapsed() > timeout {
                        let _ = child.kill();
                        std::thread::sleep(Duration::from_millis(50));
                        break;
                    }
                    
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    return Ok(vec![format!("无法等待进程: {}", e)]);
                }
            }
        }
        
        let output = child.wait_with_output()?;
        
        let mut lines = Vec::new();
        
        if !output.stdout.is_empty() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if !line.trim().is_empty() {
                    lines.push(line.to_string());
                }
            }
        }
        
        if !output.stderr.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            for line in stderr.lines() {
                if !line.trim().is_empty() {
                    lines.push(format!("\x1b[31m{}\x1b[0m", line));
                }
            }
        }
        
        if lines.is_empty() {
            lines.push("命令无输出".to_string());
        }
        
        Ok(lines)
    } else if let Some(file_path) = &config.file {
        let file = File::open(file_path)?;
        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        for line_res in reader.lines() {
            match line_res {
                Ok(line) => {
                    if !line.trim().is_empty() {
                        lines.push(line);
                    }
                }
                Err(e) => eprintln!("读取行失败: {}", e),
            }
        }
        if lines.is_empty() {
            lines.push(format!("文件 {} 为空", file_path));
        }
        Ok(lines)
    } else {
        let file = File::open("/proc/interrupts")?;
        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        for line_res in reader.lines() {
            match line_res {
                Ok(line) => {
                    if !line.trim().is_empty() {
                        lines.push(line);
                    }
                }
                Err(e) => eprintln!("读取行失败: {}", e),
            }
        }
        if lines.is_empty() {
            lines.push("/proc/interrupts 为空".to_string());
        }
        Ok(lines)
    }
}

struct DisplayState {
    scroll_y: u16,
    scroll_x: u16,
    content: Vec<String>,
    last_update: Instant,
}

impl DisplayState {
    fn new() -> Self {
        Self {
            scroll_y: 0,
            scroll_x: 0,
            content: Vec::new(),
            last_update: Instant::now(),
        }
    }
    
    fn update_content(&mut self, new_content: Vec<String>, width: u16, height: u16) {
        if new_content != self.content {
            let max_scroll_y = new_content.len().saturating_sub(height as usize) as u16;
            self.scroll_y = self.scroll_y.min(max_scroll_y);
            
            let max_scroll_x = new_content
                .iter()
                .map(|line| visual_width(line) as u16)
                .max()
                .unwrap_or(0)
                .saturating_sub(width)
                .max(0);
            self.scroll_x = self.scroll_x.min(max_scroll_x);
            
            self.content = new_content;
        }
    }
    
    fn get_display_text(&self, _width: u16, height: u16) -> Text<'static> {
        let start_y = self.scroll_y as usize;
        let end_y = (start_y + height as usize).min(self.content.len());
        
        if start_y >= end_y {
            return Text::from("没有内容可显示");
        }
        
        let mut lines = Vec::new();
        
        for line in &self.content[start_y..end_y] {
            let cropped_line = crop_line_for_scroll(line, self.scroll_x);
            
            let line_str = cropped_line.to_string();
            lines.push(Line::from(line_str));
        }
        
        Text::from(lines)
    }

    fn handle_key_event(
        &mut self,
        key_event: &KeyEvent,
        width: u16,
        height: u16,
    ) -> bool {
        if key_event.kind != KeyEventKind::Press {
            return false;
        }
        
        let max_scroll_y = self.content.len().saturating_sub(height as usize) as u16;
        let max_scroll_x = self.content
            .iter()
            .map(|line| visual_width(line) as u16)
            .max()
            .unwrap_or(0)
            .saturating_sub(width)
            .max(0);
        
        match key_event.code {
            KeyCode::Up => {
                self.scroll_y = self.scroll_y.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                self.scroll_y = (self.scroll_y + 1).min(max_scroll_y);
                true
            }
            
            KeyCode::PageUp => {
                self.scroll_y = self.scroll_y.saturating_sub(height);
                true
            }
            KeyCode::PageDown => {
                self.scroll_y = (self.scroll_y + height).min(max_scroll_y);
                true
            }
            
            KeyCode::Left => {
                self.scroll_x = self.scroll_x.saturating_sub(1);
                true
            }
            KeyCode::Right => {
                self.scroll_x = (self.scroll_x + 1).min(max_scroll_x);
                true
            }
            
            KeyCode::Home if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_y = 0;
                true
            }
            KeyCode::End if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_y = max_scroll_y;
                true
            }
            
            KeyCode::Home => {
                self.scroll_x = 0;
                true
            }
            KeyCode::End => {
                self.scroll_x = max_scroll_x;
                true
            }
            
            _ => false,
        }
    }
    
    fn should_update(&mut self, interval: Duration) -> bool {
        let now = Instant::now();
        let time_since_last_update = now.duration_since(self.last_update);
        
        if time_since_last_update >= interval {
            self.last_update += interval;
            true
        } else {
            false
        }
    }
}

fn format_interval(interval: Duration) -> String {
    let ms = interval.as_millis();
    if ms % 1000 == 0 {
        format!("{}s", ms / 1000)
    } else {
        format!("{}ms", ms)
    }
}

fn get_status_line(config: &AppConfig, _state: &DisplayState, width: u16, _height: u16) -> Line<'static> {
    let source = if let Some((cmd, args)) = &config.command {
        let full_cmd = format!("{} {}", cmd, args.join(" "));
        let max_len = (width as usize).saturating_sub(10);
        if full_cmd.len() > max_len {
            let truncated = &full_cmd[..max_len];
            format!("{}...", truncated)
        } else {
            full_cmd
        }
    } else if let Some(file) = &config.file {
        file.as_str().to_string()
    } else {
        "/proc/interrupts".to_string()
    };

    let status_text = format!("{}  {}", source, format_interval(config.interval));
    let green_span = Span::styled(
        status_text,
        Style::default().fg(Color::Green)
    );
    Line::from(green_span)
}

fn render_ui(frame: &mut Frame, config: &AppConfig, state: &DisplayState) {
    let full_area = frame.size();
    
    const STATUS_HEIGHT: u16 = 1;
    const SEPARATOR_HEIGHT: u16 = 1;
    const MIN_HEIGHT: u16 = STATUS_HEIGHT + SEPARATOR_HEIGHT + 1;

    let status_area = if full_area.height >= STATUS_HEIGHT {
        Some(Rect {
            x: 0,
            y: 0,
            width: full_area.width,
            height: STATUS_HEIGHT,
        })
    } else {
        None
    };

    let separator_area = if full_area.height >= MIN_HEIGHT {
        Some(Rect {
            x: 0,
            y: STATUS_HEIGHT,
            width: full_area.width,
            height: SEPARATOR_HEIGHT,
        })
    } else {
        None
    };

    let (content_y, content_height) = if full_area.height >= MIN_HEIGHT {
        (STATUS_HEIGHT + SEPARATOR_HEIGHT, full_area.height - STATUS_HEIGHT - SEPARATOR_HEIGHT)
    } else if full_area.height >= STATUS_HEIGHT + 1 {
        (STATUS_HEIGHT, full_area.height - STATUS_HEIGHT)
    } else {
        (0, 1)
    };
    
    let content_area = Rect {
        x: 0,
        y: content_y,
        width: full_area.width,
        height: content_height,
    };

    if let Some(area) = status_area {
        let status_line = get_status_line(config, state, content_area.width, content_area.height);
        frame.render_widget(Paragraph::new(status_line), area);
    }

    if let Some(area) = separator_area {
        let separator_line = Line::from("");
        frame.render_widget(Paragraph::new(separator_line), area);
    }

    let display_text = state.get_display_text(content_area.width, content_area.height);
    let paragraph = Paragraph::new(display_text);
    frame.render_widget(paragraph, content_area);
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;

    let mut stdout = io::stdout();
    
    execute!(stdout, EnterAlternateScreen)?;
    
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    
    terminal.show_cursor()
}

fn add_panic() {
    let orig_hook = panic::take_hook();

    panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen);
        
        orig_hook(panic_info);
    }));
}

impl App {
    fn new(config: AppConfig) -> io::Result<Self> {
        let terminal = setup_terminal()?;
        let mut state = DisplayState::new();
        
        match read_content(&config) {
            Ok(content) => {
                state.content = content;
            }
            Err(e) => {
                state.content = vec![format!("读取失败: {}", e)];
            }
        }
        
        Ok(Self {
            config,
            state,
            terminal,
        })
    }
    
    fn run(&mut self) -> io::Result<()> {
        loop {
            if self.state.should_update(self.config.interval) {
                match read_content(&self.config) {
                    Ok(new_content) => {
                        let size = self.terminal.size()?;
                        let content_height = if size.height >= 3 {
                            size.height - 2
                        } else if size.height >= 2 {
                            size.height - 1
                        } else {
                            1
                        };
                        let content_width = size.width;
                        self.state.update_content(new_content, content_width, content_height);
                    }
                    Err(e) => {
                        self.state.content = vec![format!("读取失败: {}", e)];
                    }
                }
            }
            
            self.terminal.draw(|frame| {
                render_ui(frame, &self.config, &self.state);
            })?;

            let poll_timeout = self.config.interval
                .checked_sub(Instant::now().duration_since(self.state.last_update))
                .unwrap_or(Duration::from_millis(100))
                .min(Duration::from_millis(100));

            if event::poll(poll_timeout)? {
                if let Event::Key(key_event) = event::read()? {
                    let is_ctrl_c = key_event.modifiers.contains(KeyModifiers::CONTROL) 
                        && key_event.code == KeyCode::Char('c');
                    
                    if is_ctrl_c || key_event.code == KeyCode::Char('q') {
                        break;
                    }

                    let size = self.terminal.size()?;
                    let content_height = if size.height >= 3 {
                        size.height - 2
                    } else if size.height >= 2 {
                        size.height - 1
                    } else {
                        1
                    };
                    let content_width = size.width;
                    self.state.handle_key_event(&key_event, content_width, content_height);
                }
            }
        }
        
        Ok(())
    }
    
    fn cleanup(mut self) -> io::Result<()> {
        restore_terminal(&mut self.terminal)
    }
}

fn main() -> io::Result<()> {
    add_panic();
    
    let config = parse_args();
    
    let mut app = App::new(config)?;
    app.run()?;
    app.cleanup()?;
    
    Ok(())
}
