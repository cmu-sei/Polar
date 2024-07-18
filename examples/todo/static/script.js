document.addEventListener('DOMContentLoaded', () => {
    const form = document.getElementById('todo-form');
    const todoList = document.getElementById('todo-list');

    form.addEventListener('submit', async (event) => {
        event.preventDefault();
        const todoValue = document.getElementById('todo-value').value;
        await addTodo(todoValue);
        document.getElementById('todo-value').value = '';
    });

    async function loadTodos() {
        const todos = await fetchTodos();
        renderTodos(todos);
    }

    async function fetchTodos() {
        const response = await fetch('./api/v1/todos');
        return await response.json();
    }

    function renderTodos(todos) {
        todoList.innerHTML = '';
        todos.forEach(todo => {
            const listItem = createTodoItem(todo);
            todoList.appendChild(listItem);
        });
    }

    function createTodoItem(todo) {
        const listItem = document.createElement('li');
        listItem.className = 'todo-item';
        listItem.innerHTML = `
            <input type="checkbox" ${todo.done ? 'checked disabled' : ''} onclick="markDone(${todo.id})">
            <span class="${todo.done ? 'done' : ''}">${todo.value}</span>
            <button onclick="deleteTodo(${todo.id})">Delete</button>
        `;
        return listItem;
    }

    async function addTodo(value) {
        const todos = await fetchTodos();
        const id = todos.length ? todos[todos.length - 1].id + 1 : 1;
        const newTodo = { id, value, done: false };
        await fetch('./api/v1/todos', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json'
            },
            body: JSON.stringify(newTodo)
        });
        todoList.appendChild(createTodoItem(newTodo));
    }

    window.markDone = async function(id) {
        await fetch(`./api/v1/todos/${id}`, {
            method: 'PUT'
        });
        const checkbox = document.querySelector(`input[onclick="markDone(${id})"]`);
        const span = checkbox.nextElementSibling;
        checkbox.disabled = true;
        span.classList.add('done');
    };

    window.deleteTodo = async function(id) {
        await fetch(`./api/v1/todos/${id}`, {
            method: 'DELETE'
        });
        const listItem = document.querySelector(`button[onclick="deleteTodo(${id})"]`).parentElement;
        listItem.remove();
    };

    loadTodos();
});
